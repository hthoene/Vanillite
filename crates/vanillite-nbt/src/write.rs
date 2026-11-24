//! Binary NBT writer.

use crate::{Compound, Error, List, Result, Tag, TagType, mutf8};

/// Writes a network NBT value: type byte and payload, no root name.
pub fn write_network(tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    out.push(tag.tag_type().id());
    write_payload(tag, out)
}

/// Writes a network compound (the common case for packets).
pub fn write_network_compound(c: &Compound, out: &mut Vec<u8>) -> Result<()> {
    out.push(TagType::Compound.id());
    write_compound(c, out)
}

/// Writes a file root: type byte, root name and compound payload.
pub fn write_named(name: &str, c: &Compound, out: &mut Vec<u8>) -> Result<()> {
    out.push(TagType::Compound.id());
    write_string(name, out)?;
    write_compound(c, out)
}

/// Writes a Java `writeUTF` string.
pub fn write_string(s: &str, out: &mut Vec<u8>) -> Result<()> {
    let bytes = mutf8::encode(s);
    let len = u16::try_from(bytes.len()).map_err(|_| Error::StringTooLong)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn write_len(n: usize, out: &mut Vec<u8>) -> Result<()> {
    let n = i32::try_from(n).map_err(|_| Error::InvalidLength)?;
    out.extend_from_slice(&n.to_be_bytes());
    Ok(())
}

/// Writes the payload of `tag` without its type byte.
pub fn write_payload(tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    match tag {
        Tag::Byte(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::ByteArray(v) => {
            write_len(v.len(), out)?;
            out.extend(v.iter().map(|b| b.to_be_bytes()[0]));
        }
        Tag::IntArray(v) => {
            write_len(v.len(), out)?;
            v.iter()
                .for_each(|x| out.extend_from_slice(&x.to_be_bytes()));
        }
        Tag::LongArray(v) => {
            write_len(v.len(), out)?;
            v.iter()
                .for_each(|x| out.extend_from_slice(&x.to_be_bytes()));
        }
        Tag::String(s) => write_string(s, out)?,
        Tag::List(l) => write_list(l, out)?,
        Tag::Compound(c) => write_compound(c, out)?,
    }
    Ok(())
}

fn write_list(l: &List, out: &mut Vec<u8>) -> Result<()> {
    let ty = l.element_type();
    out.push(ty.id());
    write_len(l.0.len(), out)?;
    for item in &l.0 {
        match item {
            // Like Vanilla: in a compound list, elements that are not plain compounds are
            // wrapped as {"": value} so that mixed lists and wrapper-shaped compounds round-trip.
            Tag::Compound(c) if ty != TagType::Compound || c.as_list_wrapper().is_none() => {
                write_compound(c, out)?;
            }
            _ if ty == TagType::Compound => {
                out.push(item.tag_type().id());
                write_string("", out)?;
                write_payload(item, out)?;
                out.push(TagType::End.id());
            }
            _ => write_payload(item, out)?,
        }
    }
    Ok(())
}

fn write_compound(c: &Compound, out: &mut Vec<u8>) -> Result<()> {
    for (name, value) in c.iter() {
        out.push(value.tag_type().id());
        write_string(name, out)?;
        write_payload(value, out)?;
    }
    out.push(TagType::End.id());
    Ok(())
}
