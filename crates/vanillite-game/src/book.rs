//! Book contents (`WritableBookContent`, `WrittenBookContent`): saving and signing a writable
//! book (`ServerGamePacketListenerImpl.updateBookContents` / `signBook`) and resolving the
//! pages of a written book (`WrittenBookContent.resolveForItem`).

use vanillite_data::item::ids as items;
use vanillite_protocol::codec::{DecodeLimited, Encode, Reader, write_len, write_varint};
use vanillite_protocol::components::ids;
use vanillite_protocol::text::Component;

use crate::item::ItemStack;

/// `WrittenBookContent.PAGE_LENGTH`: most characters of a resolved page as JSON.
const PAGE_LENGTH: usize = 32767;
/// Most bytes of one page component read from a stored stack.
const PAGE_NBT_LIMIT: usize = 2_097_152;

/// `has(WRITABLE_BOOK_CONTENT)`: a writable book has the component by default.
pub fn has_writable_content(stack: &ItemStack) -> bool {
    stack.component(ids::WRITABLE_BOOK_CONTENT).is_some()
        || (stack.item == items::WRITABLE_BOOK.0
            && !stack
                .patch()
                .is_some_and(|p| p.is_removed(ids::WRITABLE_BOOK_CONTENT)))
}

fn write_filterable_string(out: &mut Vec<u8>, raw: &str) {
    raw.encode(out);
    // No text filter: the filtered value is absent.
    out.push(0);
}

/// `updateBookContents`: the pages replace the writable content of the stack.
pub fn update_pages(stack: &mut ItemStack, pages: &[String]) {
    if !has_writable_content(stack) {
        return;
    }
    let mut out = Vec::new();
    write_len(&mut out, pages.len());
    for page in pages {
        write_filterable_string(&mut out, page);
    }
    stack.set_component(ids::WRITABLE_BOOK_CONTENT, out.into_boxed_slice());
}

/// `signBook`: the writable book becomes a written book by `author` with the pages as plain
/// text (`transmuteCopy(WRITTEN_BOOK)` keeps the other components). `None` when the stack has
/// no writable content.
pub fn sign(stack: &ItemStack, title: &str, author: &str, pages: &[String]) -> Option<ItemStack> {
    if !has_writable_content(stack) {
        return None;
    }
    let mut book = stack.clone();
    book.item = items::WRITTEN_BOOK.0;
    let patch = book.patch_mut();
    patch.unset(ids::WRITABLE_BOOK_CONTENT);
    patch.removed.retain(|&t| t != ids::WRITABLE_BOOK_CONTENT);
    let mut out = Vec::new();
    write_filterable_string(&mut out, title);
    author.encode(&mut out);
    write_varint(&mut out, 0);
    write_len(&mut out, pages.len());
    for page in pages {
        Component::text(page.as_str()).encode(&mut out);
        out.push(0);
    }
    // `resolved`.
    out.push(1);
    patch.set(ids::WRITTEN_BOOK_CONTENT, out.into_boxed_slice());
    Some(book)
}

/// A decoded `written_book_content`.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenContent {
    /// Raw and filtered title.
    pub title: (String, Option<String>),
    /// `author`.
    pub author: String,
    /// `generation` (0–3).
    pub generation: i32,
    /// Raw and filtered pages.
    pub pages: Vec<(Component, Option<Component>)>,
    /// `resolved`.
    pub resolved: bool,
}

impl WrittenContent {
    /// Reads the wire value of the component.
    pub fn decode(value: &[u8]) -> Option<WrittenContent> {
        let mut r = Reader::new(value);
        let string = |r: &mut Reader<'_>, max| -> Option<(String, Option<String>)> {
            let raw = r.string(max).ok()?.to_owned();
            let filtered = match r.u8().ok()? {
                0 => None,
                _ => Some(r.string(max).ok()?.to_owned()),
            };
            Some((raw, filtered))
        };
        let title = string(&mut r, 32)?;
        let author = r.string(32767).ok()?.to_owned();
        let generation = r.varint().ok()?;
        let n = r.len_prefix("book pages", 1 << 16, 2).ok()?;
        let mut pages = Vec::with_capacity(n);
        for _ in 0..n {
            let raw = Component::decode_limited(&mut r, PAGE_NBT_LIMIT).ok()?;
            let filtered = match r.u8().ok()? {
                0 => None,
                _ => Some(Component::decode_limited(&mut r, PAGE_NBT_LIMIT).ok()?),
            };
            pages.push((raw, filtered));
        }
        let resolved = r.u8().ok()? != 0;
        Some(WrittenContent {
            title,
            author,
            generation,
            pages,
            resolved,
        })
    }

    /// The wire value.
    pub fn encode(&self) -> Box<[u8]> {
        let mut out = Vec::new();
        self.title.0.encode(&mut out);
        self.title.1.encode(&mut out);
        self.author.encode(&mut out);
        write_varint(&mut out, self.generation);
        write_len(&mut out, self.pages.len());
        for (raw, filtered) in &self.pages {
            raw.encode(&mut out);
            filtered.encode(&mut out);
        }
        out.push(u8::from(self.resolved));
        out.into_boxed_slice()
    }
}

/// Whether the stack carries a written book content that is not resolved yet.
pub fn needs_resolving(stack: &ItemStack) -> bool {
    stack
        .component(ids::WRITTEN_BOOK_CONTENT)
        .and_then(WrittenContent::decode)
        .is_some_and(|c| !c.resolved)
}

/// `WrittenBookContent.resolveForItem`: each page through `resolve` (`ComponentUtils.resolve`;
/// `None` when it fails, which keeps the page). A page longer than [`PAGE_LENGTH`] as JSON
/// makes the whole resolution fail: the content is then only marked resolved. Returns whether
/// the pages changed.
pub fn resolve_for_item(
    stack: &mut ItemStack,
    resolve: &mut dyn FnMut(&Component) -> Option<Component>,
) -> bool {
    let Some(mut content) = stack
        .component(ids::WRITTEN_BOOK_CONTENT)
        .and_then(WrittenContent::decode)
    else {
        return false;
    };
    if content.resolved {
        return false;
    }
    let mut page = |c: &Component| -> Option<Component> {
        let Some(new) = resolve(c) else {
            return Some(c.clone());
        };
        (new.to_json_string().encode_utf16().count() <= PAGE_LENGTH).then_some(new)
    };
    let mut pages = Vec::with_capacity(content.pages.len());
    let mut ok = true;
    for (raw, filtered) in &content.pages {
        let Some(raw) = page(raw) else {
            ok = false;
            break;
        };
        let filtered = match filtered {
            Some(f) => match page(f) {
                Some(f) => Some(f),
                None => {
                    ok = false;
                    break;
                }
            },
            None => None,
        };
        pages.push((raw, filtered));
    }
    if ok {
        content.pages = pages;
    }
    content.resolved = true;
    stack.set_component(ids::WRITTEN_BOOK_CONTENT, content.encode());
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_makes_a_resolved_written_book() {
        let mut book = ItemStack::new(items::WRITABLE_BOOK.0, 1);
        update_pages(&mut book, &["a".to_owned()]);
        let signed = sign(&book, "T", "Alex", &["hello".to_owned()]).expect("signed");
        assert_eq!(signed.item, items::WRITTEN_BOOK.0);
        assert!(signed.component(ids::WRITABLE_BOOK_CONTENT).is_none());
        let content = WrittenContent::decode(
            signed
                .component(ids::WRITTEN_BOOK_CONTENT)
                .expect("content"),
        )
        .expect("decodes");
        assert_eq!(content.title.0, "T");
        assert_eq!(content.author, "Alex");
        assert_eq!(content.pages.len(), 1);
        assert!(content.resolved);
        assert!(!needs_resolving(&signed));
    }

    #[test]
    fn signing_needs_a_writable_book() {
        let paper = ItemStack::new(items::PAPER.0, 1);
        assert!(sign(&paper, "T", "A", &[]).is_none());
    }
}
