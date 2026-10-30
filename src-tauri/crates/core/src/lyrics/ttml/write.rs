use super::{NS_ITUNES, NS_TT, NS_TTM};
use crate::lyrics::model::{Document, Translation, Word};
use crate::lyrics::resolve::{resolve, ResolvedLine, Span, Units};
use crate::lyrics::time::format_ttml_time;
use crate::lyrics::ExportError;

/// How finely the document is timed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Timing {
    /// Word timing for each line whose units are all synced, line
    /// timing for the others.
    #[default]
    Auto,
    Line,
    Word,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteOptions {
    pub timing: Timing,
}

/// Write a TTML document.
///
/// Lines are grouped into a `<div>` per run of the same song part, keyed
/// `L1`, `L2`, … in order, and their translations and romanization go to
/// the head under those keys. A singer a line names but the document does
/// not declare is declared as a person, so a player can lay a duet out.
pub fn write(doc: &Document, options: &WriteOptions) -> Result<String, ExportError> {
    let units = match options.timing {
        Timing::Auto => Units::WhereTimed,
        Timing::Word => Units::Words,
        Timing::Line => Units::Lines,
    };
    let lines = resolve(doc, units)?;
    let word_timed = lines.iter().any(|l| l.words.is_some());
    let mut x = Xml::default();

    x.raw("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.raw(&format!(
        "<tt xmlns=\"{NS_TT}\" xmlns:ttm=\"{NS_TTM}\" xmlns:itunes=\"{NS_ITUNES}\" itunes:timing=\"{}\"",
        if word_timed { "Word" } else { "Line" }
    ));
    if let Some(lang) = &doc.metadata.language {
        x.attr("xml:lang", lang);
    }
    x.raw(">\n  <head>\n    <metadata>\n");
    write_head(&mut x, doc, &lines);
    x.raw("    </metadata>\n  </head>\n");

    x.raw("  <body");
    if let Some(end) = lines.last().and_then(|l| l.end) {
        x.attr("dur", &format_ttml_time(end));
    }
    x.raw(">\n");
    let mut k = 0;
    while k < lines.len() {
        let part = lines[k].line.part.as_deref();
        let run = lines[k..]
            .iter()
            .take_while(|l| l.line.part.as_deref() == part)
            .count();
        let group = &lines[k..k + run];
        x.raw("    <div");
        x.attr("begin", &format_ttml_time(group[0].begin));
        if let Some(end) = group.iter().map(|l| l.end).collect::<Option<Vec<_>>>() {
            if let Some(max) = end.into_iter().max() {
                x.attr("end", &format_ttml_time(max));
            }
        }
        if let Some(part) = part {
            x.attr("itunes:songPart", part);
        }
        x.raw(">\n");
        for (offset, line) in group.iter().enumerate() {
            write_line(&mut x, line, k + offset);
        }
        x.raw("    </div>\n");
        k += run;
    }
    x.raw("  </body>\n</tt>\n");
    Ok(x.0)
}

fn key(position: usize) -> String {
    format!("L{}", position + 1)
}

fn write_head(x: &mut Xml, doc: &Document, lines: &[ResolvedLine]) {
    if let Some(title) = &doc.metadata.title {
        x.raw("      <ttm:title>");
        x.text(title);
        x.raw("</ttm:title>\n");
    }
    let mut agents = doc.agents.clone();
    for line in lines {
        if let Some(id) = &line.line.agent {
            if !agents.iter().any(|a| &a.id == id) {
                agents.push(crate::lyrics::model::Agent {
                    id: id.clone(),
                    kind: crate::lyrics::model::AgentKind::Person,
                    name: None,
                });
            }
        }
    }
    for agent in &agents {
        x.raw("      <ttm:agent");
        x.attr("type", agent.kind.as_ttml());
        x.attr("xml:id", &agent.id);
        match &agent.name {
            Some(name) => {
                x.raw("><ttm:name type=\"full\">");
                x.text(name);
                x.raw("</ttm:name></ttm:agent>\n");
            }
            None => x.raw("/>\n"),
        }
    }

    // Translations grouped by language, in the order languages appear.
    let mut languages: Vec<Option<&str>> = Vec::new();
    for line in lines {
        for t in &line.line.translations {
            if !languages.contains(&t.lang.as_deref()) {
                languages.push(t.lang.as_deref());
            }
        }
    }
    let has_reading = lines.iter().any(|l| l.line.romanization.is_some());
    let writers = &doc.metadata.songwriters;
    if languages.is_empty() && !has_reading && writers.is_empty() {
        return;
    }
    x.raw(&format!("      <iTunesMetadata xmlns=\"{NS_ITUNES}\">\n"));
    if !languages.is_empty() {
        x.raw("        <translations>\n");
        for lang in &languages {
            x.raw("          <translation type=\"subtitle\"");
            if let Some(lang) = lang {
                x.attr("xml:lang", lang);
            }
            x.raw(">\n");
            for (position, line) in lines.iter().enumerate() {
                let found: Option<&Translation> = line
                    .line
                    .translations
                    .iter()
                    .find(|t| t.lang.as_deref() == *lang);
                if let Some(t) = found {
                    x.raw("            <text");
                    x.attr("for", &key(position));
                    x.raw(">");
                    x.text(&t.text);
                    x.raw("</text>\n");
                }
            }
            x.raw("          </translation>\n");
        }
        x.raw("        </translations>\n");
    }
    if has_reading {
        x.raw("        <transliterations>\n          <transliteration>\n");
        for (position, line) in lines.iter().enumerate() {
            if let Some(reading) = &line.line.romanization {
                x.raw("            <text");
                x.attr("for", &key(position));
                x.raw(">");
                x.text(reading);
                x.raw("</text>\n");
            }
        }
        x.raw("          </transliteration>\n        </transliterations>\n");
    }
    if !writers.is_empty() {
        x.raw("        <songwriters>\n");
        for writer in writers {
            x.raw("          <songwriter>");
            x.text(writer);
            x.raw("</songwriter>\n");
        }
        x.raw("        </songwriters>\n");
    }
    x.raw("      </iTunesMetadata>\n");
}

fn write_line(x: &mut Xml, r: &ResolvedLine, position: usize) {
    x.raw("      <p");
    x.attr("begin", &format_ttml_time(r.begin));
    if let Some(end) = r.end {
        x.attr("end", &format_ttml_time(end));
    }
    x.attr("itunes:key", &key(position));
    if let Some(agent) = &r.line.agent {
        x.attr("ttm:agent", agent);
    }
    x.raw(">");
    write_units(x, &r.line.words, r.words.as_deref());
    if !r.line.background.is_empty() {
        if !r.line.words.is_empty() {
            x.raw(" ");
        }
        x.raw("<span ttm:role=\"x-bg\">");
        write_units(x, &r.line.background, r.background.as_deref());
        x.raw("</span>");
    }
    x.raw("</p>\n");
}

fn write_units(x: &mut Xml, words: &[Word], spans: Option<&[Span]>) {
    let Some(spans) = spans else {
        x.text(&crate::lyrics::model::join_words(words));
        return;
    };
    for (i, (word, span)) in words.iter().zip(spans).enumerate() {
        x.raw("<span");
        x.attr("begin", &format_ttml_time(span.begin));
        x.attr("end", &format_ttml_time(span.end));
        x.raw(">");
        x.text(&word.text);
        x.raw("</span>");
        if !word.joined && i + 1 < words.len() {
            x.raw(" ");
        }
    }
}

#[derive(Default)]
struct Xml(String);

impl Xml {
    fn raw(&mut self, s: &str) {
        self.0.push_str(s);
    }

    fn attr(&mut self, name: &str, value: &str) {
        self.0.push(' ');
        self.0.push_str(name);
        self.0.push_str("=\"");
        escape_into(&mut self.0, value, true);
        self.0.push('"');
    }

    fn text(&mut self, value: &str) {
        escape_into(&mut self.0, value, false);
    }
}

fn escape_into(out: &mut String, value: &str, attribute: bool) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            // Raw newlines and tabs in an attribute are normalised to
            // spaces by every XML parser; keep them.
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#9;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' && c != '\r' => {}
            c => out.push(c),
        }
    }
}
