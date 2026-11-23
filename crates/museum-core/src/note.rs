//! Markdown notes: a card for the wall, and the note rendered to HTML with
//! its links pointed at the files they name.
//!
//! Notes are plain Markdown, written in Obsidian or anywhere else, hung
//! beside the photos and PDFs they describe. The wall shows a card: the first
//! heading as a title, and the opening lines. Opened, the note is converted
//! here on the server, so that its links can be resolved against the library
//! index and the browser receives ids rather than paths (`docs/design.md`
//! §6.8).
//!
//! Links are resolved by file name, not by path. A note's links point at
//! wherever the files sat in the vault, which has nothing to do with the
//! folders they were uploaded into; Obsidian itself finds a link's target by
//! name, so this follows it.

use std::collections::HashMap;
use std::path::Path;

use comrak::nodes::{AstNode, NodeLink, NodeValue};
use comrak::{Arena, Options, format_html, parse_document};
use unicode_normalization::UnicodeNormalization;

use crate::id::ImageId;
use crate::library::{ImageEntry, Kind};
use crate::probe::{Probe, ProbeError, modified_of, version_of};

/// A note on the wall is drawn as a card of this shape, a portrait page.
const CARD_WIDTH: u32 = 300;
const CARD_HEIGHT: u32 = 400;
/// How much of the note's opening text the card carries.
const EXCERPT_CHARS: usize = 200;

/// What the wall shows of a note.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    /// The first heading, if the note has one.
    pub title: Option<String>,
    /// The opening text, without markup, cut to a couple of lines.
    pub excerpt: String,
}

fn options() -> Options<'static> {
    let mut options = Options::default();
    let extension = &mut options.extension;
    extension.table = true;
    extension.tasklist = true;
    extension.strikethrough = true;
    extension.autolink = true;
    extension.footnotes = true;
    // `[[target|label]]`, as Obsidian writes it.
    extension.wikilinks_title_after_pipe = true;
    // A leading `---` block of YAML is Obsidian's properties, not prose.
    extension.front_matter_delimiter = Some("---".to_owned());
    // Raw HTML in a note is passed through here and cleaned by ammonia
    // afterwards, which is the one place that decides what is safe.
    options.render.r#unsafe = true;
    options
}

pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let metadata = std::fs::metadata(path).map_err(ProbeError::Metadata)?;
    let text = read(path).map_err(ProbeError::Open)?;
    Ok(Probe {
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        version: version_of(&metadata),
        duration: None,
        taken: None,
        modified: modified_of(&metadata),
        card: Some(card(&text)),
    })
}

/// Notes are text, but one saved in another encoding should still show what
/// it can rather than vanish from the wall.
pub fn read(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn card(markdown: &str) -> Card {
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options());
    let mut card = Card::default();
    for node in root.descendants() {
        match &node.data().value {
            NodeValue::Heading(_) if card.title.is_none() => {
                card.title = Some(plain_text(node)).filter(|title| !title.is_empty());
            }
            NodeValue::Paragraph if card.excerpt.chars().count() < EXCERPT_CHARS => {
                let text = plain_text(node);
                if !text.is_empty() {
                    if !card.excerpt.is_empty() {
                        card.excerpt.push(' ');
                    }
                    card.excerpt.push_str(&text);
                }
            }
            _ => {}
        }
    }
    if card.excerpt.chars().count() > EXCERPT_CHARS {
        card.excerpt = card.excerpt.chars().take(EXCERPT_CHARS).collect::<String>() + "…";
    }
    card
}

fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut text = String::new();
    for inner in node.descendants() {
        match &inner.data().value {
            NodeValue::Text(t) => text.push_str(t),
            NodeValue::Code(code) => text.push_str(&code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
            NodeValue::WikiLink(link) if inner.first_child().is_none() => text.push_str(&link.url),
            _ => {}
        }
    }
    text.trim().to_owned()
}

/// A file a link can point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: ImageId,
    pub kind: Kind,
    pub v: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Found(Target),
    /// No file of that name hangs in any room.
    Missing,
    /// Several do, none in the note's own room, so none is picked: guessing
    /// would quietly show the wrong file.
    Ambiguous(usize),
}

/// Every file hung in a room, by normalized file name.
#[derive(Debug, Default)]
pub struct LinkIndex {
    by_name: HashMap<String, Vec<(String, Target)>>,
}

impl LinkIndex {
    /// Indexes what hangs in the rooms. Only files in a room can be linked
    /// to: anything else could not be shown.
    pub fn new<'a>(rooms: impl IntoIterator<Item = (&'a str, &'a [ImageEntry])>) -> Self {
        let mut by_name: HashMap<String, Vec<(String, Target)>> = HashMap::new();
        for (room, entries) in rooms {
            for entry in entries {
                by_name
                    .entry(normalize_name(&entry.name))
                    .or_default()
                    .push((
                        room.to_owned(),
                        Target {
                            id: entry.id,
                            kind: entry.kind,
                            v: entry.v.clone(),
                        },
                    ));
            }
        }
        Self { by_name }
    }

    /// Finds the file a link names, from a note hanging in `room`. A file in
    /// the same room wins; otherwise the name must be unique in the library.
    pub fn resolve(&self, name: &str, room: &str) -> Resolution {
        let Some(candidates) = self.by_name.get(&normalize_name(name)) else {
            return Resolution::Missing;
        };
        if let Some((_, target)) = candidates.iter().find(|(r, _)| r == room) {
            return Resolution::Found(target.clone());
        }
        match candidates.as_slice() {
            [(_, only)] => Resolution::Found(only.clone()),
            _ => Resolution::Ambiguous(candidates.len()),
        }
    }
}

/// How names are compared. NFC first: a name typed on a Mac or an iPhone
/// stores a voiced kana as two code points, and looks the same but does not
/// match the one-code-point spelling. Then case is folded, as Obsidian does.
pub fn normalize_name(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// A link that did not lead anywhere, for `lumier check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The file name the link asked for.
    pub name: String,
    pub resolution: Resolution,
}

pub struct Rendered {
    pub html: String,
    pub issues: Vec<Issue>,
}

/// Where a link points, before it is looked up.
enum Destination {
    /// Another site, or a mail address: left as written.
    External,
    /// A heading within this note: left as written.
    Fragment,
    /// A file, named by its last path segment, with any `#page=3`.
    File { name: String, fragment: String },
}

fn destination(url: &str) -> Destination {
    if url.is_empty() || url.starts_with('#') {
        return Destination::Fragment;
    }
    if url.contains("://") || url.starts_with("mailto:") {
        return Destination::External;
    }
    let (path, fragment) = match url.split_once('#') {
        Some((path, fragment)) => (path, format!("#{fragment}")),
        None => (url, String::new()),
    };
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    // `[[Trip]]` names the note `Trip.md`, as Obsidian reads it.
    let name = if Path::new(&name).extension().is_none() {
        format!("{name}.md")
    } else {
        name
    };
    Destination::File { name, fragment }
}

/// Renders a note to HTML safe to put into the page.
///
/// Every link and embed that names a file is looked up with `resolve`. A
/// found link points at the file by id; an embedded picture becomes its
/// thumbnail, an embedded video a player. A link that leads nowhere keeps
/// its text, marked so it can be seen to be broken.
pub fn render(markdown: &str, resolve: &dyn Fn(&str) -> Resolution) -> Rendered {
    let arena = Arena::new();
    let options = options();
    let root = parse_document(&arena, markdown, &options);
    let mut issues = Vec::new();

    // Collected first: the rewrites below move nodes around, which would
    // upset a walk still in progress.
    let links: Vec<_> = root
        .descendants()
        .filter(|node| {
            matches!(
                node.data().value,
                NodeValue::Link(_) | NodeValue::Image(_) | NodeValue::WikiLink(_)
            )
        })
        .collect();

    for node in links {
        let (url, embed) = match &node.data().value {
            NodeValue::Link(link) => (link.url.clone(), false),
            NodeValue::Image(link) => (link.url.clone(), true),
            NodeValue::WikiLink(link) => (link.url.clone(), link.embed),
            _ => continue,
        };
        let Destination::File { name, fragment } = destination(&url) else {
            continue;
        };
        match resolve(&name) {
            Resolution::Found(target) => point_at(&arena, node, &target, &fragment, embed),
            resolution => {
                mark_broken(&arena, node);
                issues.push(Issue { name, resolution });
            }
        }
    }

    let mut html = String::new();
    format_html(root, &options, &mut html).expect("writing to a String cannot fail");
    Rendered {
        html: sanitize(&html),
        issues,
    }
}

fn point_at<'a>(
    arena: &'a Arena<'a>,
    node: &'a AstNode<'a>,
    target: &Target,
    fragment: &str,
    embed: bool,
) {
    let original = format!("/img/{}/orig?v={}{fragment}", target.id, target.v);
    let value = match (embed, target.kind) {
        (true, Kind::Image | Kind::Pdf) => NodeValue::Image(Box::new(NodeLink::new(
            format!("/img/{}/1280?v={}", target.id, target.v),
            "",
        ))),
        (true, Kind::Video) => {
            for child in node.children().collect::<Vec<_>>() {
                child.detach();
            }
            NodeValue::HtmlInline(format!(
                "<video controls preload=\"metadata\" src=\"{original}\"></video>"
            ))
        }
        // A note cannot be embedded in another (§6.8); it is linked instead.
        _ => NodeValue::Link(Box::new(NodeLink::new(original, ""))),
    };
    // A wiki link with no label shows its target; give the replacement that
    // text, since a plain link or an image has no other.
    let label = match &node.data().value {
        NodeValue::WikiLink(link) => {
            Some(link.url.split('#').next().unwrap_or_default().to_owned())
        }
        _ => None,
    };
    if let Some(label) = label
        && node.first_child().is_none()
        && !matches!(value, NodeValue::HtmlInline(_))
    {
        node.append(arena.alloc(AstNode::from(NodeValue::Text(label.into()))));
    }
    node.data_mut().value = value;
}

/// Swaps a link that leads nowhere for its own text in a marked span.
fn mark_broken<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>) {
    let open = arena.alloc(AstNode::from(NodeValue::HtmlInline(
        "<span class=\"link--broken\">".to_owned(),
    )));
    let close = arena.alloc(AstNode::from(NodeValue::HtmlInline("</span>".to_owned())));
    node.insert_before(open);
    let children: Vec<_> = node.children().collect();
    if children.is_empty() {
        let label = link_url(&node.data().value);
        node.insert_before(arena.alloc(AstNode::from(NodeValue::Text(label.into()))));
    }
    for child in children {
        child.detach();
        node.insert_before(child);
    }
    node.insert_before(close);
    node.detach();
}

fn link_url(value: &NodeValue) -> String {
    match value {
        NodeValue::Link(link) | NodeValue::Image(link) => link.url.clone(),
        NodeValue::WikiLink(link) => link.url.clone(),
        _ => String::new(),
    }
}

/// The one place that decides what HTML may reach the page.
fn sanitize(html: &str) -> String {
    ammonia::Builder::default()
        .add_tags(["video", "input"])
        .add_tag_attributes("video", ["src", "controls", "preload"])
        // Task list checkboxes, rendered disabled.
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .add_allowed_classes("span", ["link--broken"])
        // Every link opens in a new tab: the client catches the ones that
        // lead to something it can show itself.
        .set_tag_attribute_value("a", "target", "_blank")
        .link_rel(Some("noopener noreferrer"))
        .clean(html)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(n: u8, kind: Kind) -> Target {
        Target {
            id: ImageId::for_rel_path(&n.to_string()),
            kind,
            v: format!("v{n}"),
        }
    }

    fn resolver(name: &str) -> Resolution {
        match name {
            "資料.pdf" => Resolution::Found(target(1, Kind::Pdf)),
            "写真.png" => Resolution::Found(target(2, Kind::Image)),
            "clip.mp4" => Resolution::Found(target(3, Kind::Video)),
            "trip.md" | "Trip.md" => Resolution::Found(target(4, Kind::Markdown)),
            "both.jpg" => Resolution::Ambiguous(2),
            _ => Resolution::Missing,
        }
    }

    #[test]
    fn takes_the_first_heading_as_title_and_skips_front_matter() {
        let card = card("---\ntags: [x]\n---\n\nIntro line.\n\n# The trip\n\nWe went **north**.\n");
        assert_eq!(card.title.as_deref(), Some("The trip"));
        assert_eq!(card.excerpt, "Intro line. We went north.");
    }

    #[test]
    fn cuts_a_long_excerpt() {
        let card = card(&"word ".repeat(100));
        assert_eq!(card.excerpt.chars().count(), EXCERPT_CHARS + 1);
        assert!(card.excerpt.ends_with('…'));
    }

    #[test]
    fn points_links_of_every_kind_at_ids() {
        let id = |n: u8| ImageId::for_rel_path(&n.to_string());
        let rendered = render(
            "[the doc](attachments/%E8%B3%87%E6%96%99.pdf#page=3) and [[写真.png]] and [[Trip|our trip]]",
            &resolver,
        );
        let html = &rendered.html;
        assert!(
            html.contains(&format!("href=\"/img/{}/orig?v=v1#page=3\"", id(1))),
            "{html}"
        );
        assert!(
            html.contains(&format!("href=\"/img/{}/orig?v=v2\"", id(2))),
            "{html}"
        );
        assert!(html.contains(">写真.png</a>"), "{html}");
        assert!(
            html.contains(&format!("href=\"/img/{}/orig?v=v4\"", id(4))),
            "{html}"
        );
        assert!(html.contains(">our trip</a>"), "{html}");
        assert!(html.contains("target=\"_blank\""), "{html}");
        assert!(rendered.issues.is_empty());
    }

    #[test]
    fn embeds_pictures_as_thumbnails_and_videos_as_players() {
        let id = |n: u8| ImageId::for_rel_path(&n.to_string());
        let html = render("![[写真.png]]\n\n![](clip.mp4)\n", &resolver).html;
        assert!(
            html.contains(&format!("<img src=\"/img/{}/1280?v=v2\"", id(2))),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                "<video controls=\"\" preload=\"metadata\" src=\"/img/{}/orig?v=v3\"",
                id(3)
            )),
            "{html}"
        );
    }

    #[test]
    fn marks_links_that_lead_nowhere_and_reports_them() {
        let rendered = render("See [[gone.pdf]] and [this](both.jpg).", &resolver);
        let html = &rendered.html;
        assert!(
            html.contains("<span class=\"link--broken\">gone.pdf</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"link--broken\">this</span>"),
            "{html}"
        );
        assert_eq!(
            rendered.issues,
            [
                Issue {
                    name: "gone.pdf".into(),
                    resolution: Resolution::Missing
                },
                Issue {
                    name: "both.jpg".into(),
                    resolution: Resolution::Ambiguous(2)
                },
            ]
        );
    }

    #[test]
    fn leaves_external_links_and_strips_scripts() {
        let html = render(
            "[site](https://example.com) [top](#top)\n\n<script>alert(1)</script><b onclick=\"x()\">bold</b>",
            &resolver,
        )
        .html;
        assert!(html.contains("href=\"https://example.com\""), "{html}");
        assert!(html.contains("href=\"#top\""), "{html}");
        assert!(!html.contains("script"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
        assert!(html.contains("<b>bold</b>"), "{html}");
    }

    #[test]
    fn renders_tables_and_task_lists() {
        let html = render(
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n",
            &resolver,
        )
        .html;
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("type=\"checkbox\""), "{html}");
    }

    #[test]
    fn prefers_the_same_room_then_a_unique_name() {
        let entry = |name: &str, n: u8| ImageEntry {
            id: ImageId::for_rel_path(&n.to_string()),
            name: name.to_owned(),
            w: 1,
            h: 1,
            v: String::new(),
            kind: Kind::Image,
            duration: None,
            title: None,
            excerpt: None,
            rel_path: String::new(),
        };
        let a = [entry("Same.jpg", 1), entry("only.jpg", 3)];
        let b = [entry("same.jpg", 2)];
        let index = LinkIndex::new([("a", &a[..]), ("b", &b[..])]);
        let found = |name, room| match index.resolve(name, room) {
            Resolution::Found(target) => Some(target.id),
            _ => None,
        };
        assert_eq!(found("same.jpg", "b"), Some(ImageId::for_rel_path("2")));
        assert_eq!(found("SAME.jpg", "a"), Some(ImageId::for_rel_path("1")));
        assert_eq!(index.resolve("same.jpg", "c"), Resolution::Ambiguous(2));
        assert_eq!(found("only.jpg", "b"), Some(ImageId::for_rel_path("3")));
        assert_eq!(index.resolve("nothing.jpg", "a"), Resolution::Missing);
    }

    #[test]
    fn matches_a_decomposed_name() {
        // "ガ" as カ plus a combining voiced mark, as macOS writes it.
        assert_eq!(
            normalize_name("\u{30AB}\u{3099}.pdf"),
            normalize_name("ガ.pdf")
        );
    }
}
