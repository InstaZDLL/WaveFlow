use std::collections::HashMap;

use roxmltree::{Node, NodeType};

use crate::lyrics::model::{Agent, AgentKind, Document, Line, Translation, Word};
use crate::lyrics::time::parse_ttml_time;
use crate::lyrics::ParseError;

/// Read a TTML document.
///
/// Every `<p>` under `<body>` becomes a line. Inside it, each `<span>`
/// with a time is a unit; whitespace between two spans separates words
/// and its absence glues syllables. Bare text (a line-timed document) is
/// split into untimed words. Translations and transliterations come from
/// the head, matched by `itunes:key`, or from inline `x-translation` /
/// `x-roman` spans.
pub fn parse(xml: &str) -> Result<Document, ParseError> {
    let tree = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}'))
        .map_err(|e| ParseError::Xml(e.to_string()))?;
    let root = tree.root_element();
    if root.tag_name().name() != "tt" {
        return Err(ParseError::NotTtml(root.tag_name().name().to_string()));
    }

    let mut doc = Document::default();
    doc.metadata.language = attr(root, "lang").map(str::to_string);
    let mut translations: HashMap<String, Vec<Translation>> = HashMap::new();
    let mut readings: HashMap<String, String> = HashMap::new();

    if let Some(head) = root.children().find(|n| is(n, "head")) {
        for agent in head.descendants().filter(|n| is(n, "agent")) {
            let Some(id) = attr(agent, "id") else {
                continue;
            };
            doc.agents.push(Agent {
                id: id.to_string(),
                kind: AgentKind::from_ttml(attr(agent, "type").unwrap_or("person")),
                name: agent
                    .children()
                    .find(|n| is(n, "name"))
                    .map(text_of)
                    .filter(|s| !s.is_empty()),
            });
        }
        doc.metadata.title = head
            .descendants()
            .find(|n| is(n, "title"))
            .map(text_of)
            .filter(|s| !s.is_empty());
        doc.metadata.songwriters = head
            .descendants()
            .filter(|n| is(n, "songwriter"))
            .map(text_of)
            .filter(|s| !s.is_empty())
            .collect();
        for container in head.descendants().filter(|n| is(n, "translation")) {
            let lang = attr(container, "lang").map(str::to_string);
            for entry in container.children().filter(|n| is(n, "text")) {
                let (Some(key), text) = (attr(entry, "for"), text_of(entry)) else {
                    continue;
                };
                if !text.is_empty() {
                    translations
                        .entry(key.to_string())
                        .or_default()
                        .push(Translation {
                            lang: lang.clone(),
                            text,
                        });
                }
            }
        }
        // Only the first transliteration: the model holds one reading.
        if let Some(container) = head.descendants().find(|n| is(n, "transliteration")) {
            for entry in container.children().filter(|n| is(n, "text")) {
                if let Some(key) = attr(entry, "for") {
                    let text = text_of(entry);
                    if !text.is_empty() {
                        readings.entry(key.to_string()).or_insert(text);
                    }
                }
            }
        }
    }

    let Some(body) = root.children().find(|n| is(n, "body")) else {
        return Ok(doc);
    };
    for p in body.descendants().filter(|n| is(n, "p")) {
        let mut content = Content::default();
        walk(p, &mut content, false);
        finish(&mut content.words);
        finish(&mut content.background);
        if content.words.is_empty() && content.background.is_empty() {
            continue;
        }
        // Backing vocals with no lead in their own `<p>` are sung over the
        // line before: no format writes a line without words, so kept on
        // their own they would be lost on the next save.
        if content.words.is_empty() {
            if let Some(previous) = doc.lines.last_mut() {
                previous.background.append(&mut content.background);
                if let (Some(end), Some(last)) = (previous.end_ms, previous.background.last()) {
                    previous.end_ms = Some(end.max(last.end_ms.unwrap_or(end)));
                }
                continue;
            }
        }
        let key = attr(p, "key");
        let mut line_translations = key.and_then(|k| translations.remove(k)).unwrap_or_default();
        line_translations.extend(content.translations);
        doc.lines.push(Line {
            begin_ms: attr(p, "begin").and_then(parse_ttml_time),
            end_ms: attr(p, "end").and_then(parse_ttml_time),
            agent: inherited(p, &["agent"]).map(str::to_string),
            part: inherited(p, &["songPart", "song-part"]).map(str::to_string),
            words: content.words,
            background: content.background,
            translations: line_translations,
            romanization: key
                .and_then(|k| readings.remove(k))
                .or(content.romanization),
        });
    }
    Ok(doc)
}

#[derive(Default)]
struct Content {
    words: Vec<Word>,
    background: Vec<Word>,
    translations: Vec<Translation>,
    romanization: Option<String>,
}

fn walk(node: Node, content: &mut Content, in_background: bool) {
    for child in node.children() {
        match child.node_type() {
            NodeType::Text => {
                let target = if in_background {
                    &mut content.background
                } else {
                    &mut content.words
                };
                push_text(target, child.text().unwrap_or_default());
            }
            NodeType::Element if is(&child, "br") => {
                let target = if in_background {
                    &mut content.background
                } else {
                    &mut content.words
                };
                if let Some(last) = target.last_mut() {
                    last.joined = false;
                }
            }
            NodeType::Element if is(&child, "span") => match attr(child, "role") {
                Some("x-bg") if !in_background => {
                    if let Some(last) = content.words.last_mut() {
                        last.joined = false;
                    }
                    walk(child, content, true);
                    if let Some(last) = content.background.last_mut() {
                        last.joined = false;
                    }
                }
                Some("x-translation") => {
                    let text = text_of(child);
                    if !text.is_empty() {
                        content.translations.push(Translation {
                            lang: attr(child, "lang").map(str::to_string),
                            text,
                        });
                    }
                }
                Some("x-roman") | Some("x-romanization") => {
                    let text = text_of(child);
                    if !text.is_empty() {
                        content.romanization = Some(text);
                    }
                }
                _ if child.children().any(|n| is(&n, "span")) => {
                    walk(child, content, in_background);
                }
                _ => {
                    let target = if in_background {
                        &mut content.background
                    } else {
                        &mut content.words
                    };
                    push_unit(
                        target,
                        &child
                            .text()
                            .map(str::to_string)
                            .unwrap_or_else(|| text_of(child)),
                        attr(child, "begin").and_then(parse_ttml_time),
                        attr(child, "end").and_then(parse_ttml_time),
                    );
                }
            },
            _ => {}
        }
    }
}

/// Bare text: whitespace separates words, words carry no time.
fn push_text(words: &mut Vec<Word>, text: &str) {
    if text.starts_with(char::is_whitespace) {
        if let Some(last) = words.last_mut() {
            last.joined = false;
        }
    }
    let pieces: Vec<&str> = text.split_whitespace().collect();
    for (i, piece) in pieces.iter().enumerate() {
        words.push(Word {
            text: (*piece).to_string(),
            begin_ms: None,
            end_ms: None,
            joined: i + 1 == pieces.len() && !text.ends_with(char::is_whitespace),
        });
    }
}

/// A timed span: one unit, glued to the next until whitespace says not.
fn push_unit(words: &mut Vec<Word>, raw: &str, begin: Option<u64>, end: Option<u64>) {
    let text = raw.trim();
    if text.is_empty() {
        push_text(words, raw);
        return;
    }
    if raw.starts_with(char::is_whitespace) {
        if let Some(last) = words.last_mut() {
            last.joined = false;
        }
    }
    words.push(Word {
        text: text.to_string(),
        begin_ms: begin,
        end_ms: end,
        joined: !raw.ends_with(char::is_whitespace),
    });
}

fn finish(words: &mut [Word]) {
    if let Some(last) = words.last_mut() {
        last.joined = false;
    }
}

fn is(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

/// An attribute by local name, whatever its namespace.
fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}

/// The attribute on `node` or its nearest ancestor that has it.
fn inherited<'a>(node: Node<'a, '_>, names: &[&str]) -> Option<&'a str> {
    node.ancestors()
        .filter(|n| n.is_element())
        .find_map(|n| names.iter().find_map(|name| attr(n, name)))
}

/// All the text under `node`, whitespace collapsed.
fn text_of(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
