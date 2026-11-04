//! What `roxmltree` is not asked to read.
//!
//! Its node ceiling counts neither attributes nor namespaces, and it spends
//! quadratic time on both: every attribute of a tag is compared to the ones before
//! it, and every element declaring a namespace copies the ones in scope, checking
//! each against its own. A feed of 930 KiB holding one tag with some 90 000
//! attributes took 13 s, and a root declaring 5 000 namespaces over 400 children
//! declaring one more took 12 s for 82 KiB, in a release build.
//!
//! A deadline cannot help: parsing is not a future, nothing can stop it once it runs,
//! and the pass that started it stays held. So the document is looked at first, by a
//! scan that is linear whatever it is given, and one past these limits is refused
//! without being parsed.
//!
//! The limits sit far above what real documents use. Fourteen public feeds, from a
//! few KiB to 20 MiB, used at most 9 attributes on a tag, 8 namespaces in scope and 13
//! copies in the whole document; TTML lyrics use fewer.
//!
//! The scan reads tags as `roxmltree` does, so nothing can be hidden from it in a
//! comment or a quoted value that the parser would read as markup. What it cannot
//! read as XML it refuses, since the parser would too.

/// The most attributes one tag may carry, namespace declarations included.
const MAX_ATTRIBUTES_PER_TAG: usize = 32;

/// The most attributes one document may carry. Each takes room in the tree whatever
/// its size in the text, and the largest feed read could otherwise hold over six million
/// of them. The 20 MiB feed that sized this holds a few hundred thousand tags.
const MAX_ATTRIBUTES_PER_DOCUMENT: usize = 2_000_000;

/// The most namespaces in scope at any element.
const MAX_NAMESPACES_IN_SCOPE: usize = 32;

/// The most namespace entries a document may make the parser copy, each element
/// declaring one copying all that are in scope there. A feed declaring one on each
/// of 3 000 items, under 9 at its root, stays well inside this.
const MAX_NAMESPACE_COPIES: usize = 100_000;

/// Whether `xml` may be handed to `roxmltree`. Linear in its length.
pub fn within_limits(xml: &str) -> bool {
    let bytes = xml.as_bytes();
    // The namespaces in scope at each open element, with the document at the bottom.
    let mut scopes: Vec<usize> = vec![0];
    let mut copies = 0usize;
    let mut attributes = 0usize;
    let mut at = 0usize;
    while let Some(found) = xml[at..].find('<') {
        let open = at + found;
        let rest = &xml[open..];
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else {
                return false;
            };
            at = open + end + 3;
        } else if rest.starts_with("<![CDATA[") {
            let Some(end) = rest.find("]]>") else {
                return false;
            };
            at = open + end + 3;
        } else if rest.starts_with("<?") {
            let Some(end) = rest.find("?>") else {
                return false;
            };
            at = open + end + 2;
        } else if rest.starts_with("<!") {
            // A DTD, which nothing here accepts anyway.
            return false;
        } else if rest.starts_with("</") {
            let Some(end) = rest.find('>') else {
                return false;
            };
            if scopes.len() > 1 {
                scopes.pop();
            }
            at = open + end + 1;
        } else {
            let Some(tag) = read_start_tag(bytes, open + 1) else {
                return false;
            };
            attributes += tag.attributes;
            if tag.attributes > MAX_ATTRIBUTES_PER_TAG || attributes > MAX_ATTRIBUTES_PER_DOCUMENT {
                return false;
            }
            let scope = scopes.last().copied().unwrap_or(0) + tag.declarations;
            if scope > MAX_NAMESPACES_IN_SCOPE {
                return false;
            }
            if tag.declarations > 0 {
                copies += scope;
                if copies > MAX_NAMESPACE_COPIES {
                    return false;
                }
            }
            if !tag.self_closing {
                scopes.push(scope);
            }
            at = tag.end;
        }
    }
    true
}

struct StartTag {
    attributes: usize,
    declarations: usize,
    self_closing: bool,
    /// Just past its `>`.
    end: usize,
}

/// The start tag whose name begins at `at`. Stops counting at nothing: the caller
/// decides, and the walk over one tag only ever moves forward.
fn read_start_tag(bytes: &[u8], mut at: usize) -> Option<StartTag> {
    let name_start = at;
    while at < bytes.len() && !is_space(bytes[at]) && !matches!(bytes[at], b'/' | b'>') {
        at += 1;
    }
    if at == name_start {
        return None;
    }
    let mut tag = StartTag {
        attributes: 0,
        declarations: 0,
        self_closing: false,
        end: 0,
    };
    loop {
        at = skip_space(bytes, at);
        match *bytes.get(at)? {
            b'>' => {
                tag.end = at + 1;
                return Some(tag);
            }
            b'/' => {
                if *bytes.get(at + 1)? != b'>' {
                    return None;
                }
                tag.self_closing = true;
                tag.end = at + 2;
                return Some(tag);
            }
            _ => {}
        }
        let name_start = at;
        while at < bytes.len() && !is_space(bytes[at]) && bytes[at] != b'=' {
            at += 1;
        }
        let name = &bytes[name_start..at];
        if name.is_empty() {
            return None;
        }
        at = skip_space(bytes, at);
        if *bytes.get(at)? != b'=' {
            return None;
        }
        at = skip_space(bytes, at + 1);
        let quote = *bytes.get(at)?;
        if !matches!(quote, b'"' | b'\'') {
            return None;
        }
        at += 1;
        at += bytes[at..].iter().position(|byte| *byte == quote)? + 1;
        tag.attributes += 1;
        if name == b"xmlns" || name.starts_with(b"xmlns:") {
            tag.declarations += 1;
        }
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && is_space(bytes[at]) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests;
