use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, LitInt, LitStr, Type};

struct PacketAttr {
    state: syn::Ident,
    dir: syn::Ident,
    name: LitStr,
    cost: LitInt,
    bucket: LitStr,
}

#[derive(Default)]
struct FieldAttr {
    varint: bool,
    varlong: bool,
    rest: bool,
    max_len: Option<syn::Expr>,
    finite: bool,
    range: Option<syn::ExprRange>,
}

/// Types that carry an unbounded amount of data and therefore need `max_len`.
const LIMITED: &[&str] = &[
    "String",
    "Vec",
    "Identifier",
    "RawBytes",
    "Nbt",
    "RawNbt",
    "Component",
    "BitSet",
];

fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(p) => p.path.segments.last(),
        _ => None,
    }
}

/// Whether the type (or the type inside an `Option`) needs a length limit.
fn needs_limit(ty: &Type) -> bool {
    let Some(seg) = last_segment(ty) else {
        return false;
    };
    if seg.ident == "Option" {
        if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
            && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
        {
            return needs_limit(inner);
        }
        return false;
    }
    // Identifiers have a fixed Vanilla limit and decode without an explicit one.
    seg.ident != "Identifier" && LIMITED.iter().any(|l| seg.ident == l)
}

fn parse_packet_attr(input: &DeriveInput) -> syn::Result<PacketAttr> {
    let mut state = None;
    let mut dir = None;
    let mut name = None;
    let mut cost = LitInt::new("1", Span::call_site());
    let mut bucket = LitStr::new("default", Span::call_site());
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("packet")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("state") {
                state = Some(meta.value()?.parse::<syn::Ident>()?);
            } else if meta.path.is_ident("dir") {
                dir = Some(meta.value()?.parse::<syn::Ident>()?);
            } else if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("cost") {
                cost = meta.value()?.parse()?;
            } else if meta.path.is_ident("bucket") {
                bucket = meta.value()?.parse()?;
            } else {
                return Err(meta.error("unknown packet attribute"));
            }
            Ok(())
        })?;
    }
    let span = input.ident.span();
    Ok(PacketAttr {
        state: state.ok_or_else(|| syn::Error::new(span, "missing `state` in #[packet(...)]"))?,
        dir: dir.ok_or_else(|| syn::Error::new(span, "missing `dir` in #[packet(...)]"))?,
        name: name.ok_or_else(|| syn::Error::new(span, "missing `name` in #[packet(...)]"))?,
        cost,
        bucket,
    })
}

fn parse_field_attr(field: &syn::Field) -> syn::Result<FieldAttr> {
    let mut fa = FieldAttr::default();
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("field")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("varint") {
                fa.varint = true;
            } else if meta.path.is_ident("varlong") {
                fa.varlong = true;
            } else if meta.path.is_ident("rest") {
                fa.rest = true;
            } else if meta.path.is_ident("finite") {
                fa.finite = true;
            } else if meta.path.is_ident("max_len") {
                fa.max_len = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("range") {
                let lit: LitStr = meta.value()?.parse()?;
                fa.range = Some(lit.parse()?);
            } else {
                return Err(meta.error("unknown field attribute"));
            }
            Ok(())
        })?;
    }
    Ok(fa)
}

fn to_camel(s: &str) -> String {
    s.split(['_', '/'])
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let attr = parse_packet_attr(input)?;
    let ident = &input.ident;
    let krate = quote!(::vanillite_protocol);

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            ident.span(),
            "#[derive(Packet)] supports structs only",
        ));
    };

    let mut encodes = Vec::new();
    let mut decodes = Vec::new();
    let mut names = Vec::new();
    for (i, field) in data.fields.iter().enumerate() {
        let fa = parse_field_attr(field)?;
        let ty = &field.ty;
        let name = field.ident.clone().unwrap_or_else(|| format_ident!("f{i}"));
        let access = match &field.ident {
            Some(id) => quote!(self.#id),
            None => {
                let idx = syn::Index::from(i);
                quote!(self.#idx)
            }
        };
        if needs_limit(ty) && fa.max_len.is_none() {
            return Err(syn::Error::new_spanned(
                ty,
                "strings, lists, byte arrays, NBT and components need #[field(max_len = N)] (SEC-02)",
            ));
        }
        let (enc, dec) = if fa.varint {
            (
                quote!(#krate::codec::write_varint(out, #access);),
                quote!(r.varint()?),
            )
        } else if fa.varlong {
            (
                quote!(#krate::codec::write_varlong(out, #access);),
                quote!(r.varlong()?),
            )
        } else if fa.rest {
            let Some(max) = &fa.max_len else {
                return Err(syn::Error::new_spanned(ty, "#[field(rest)] needs max_len"));
            };
            (
                quote!(#krate::codec::Rest::encode_rest(&#access, out);),
                quote!(<#ty as #krate::codec::Rest>::decode_rest(r, #max)?),
            )
        } else if let Some(max) = &fa.max_len {
            (
                quote!(#krate::codec::Encode::encode(&#access, out);),
                quote!(<#ty as #krate::codec::DecodeLimited>::decode_limited(r, #max)?),
            )
        } else {
            (
                quote!(#krate::codec::Encode::encode(&#access, out);),
                quote!(<#ty as #krate::codec::Decode>::decode(r)?),
            )
        };
        let mut checks = Vec::new();
        if fa.finite {
            checks.push(quote! {
                if !#name.is_finite() {
                    return Err(#krate::codec::Error::Invalid(concat!(stringify!(#ident), ".", stringify!(#name), " is not finite")));
                }
            });
        }
        if let Some(range) = &fa.range {
            checks.push(quote! {
                if !(#range).contains(&#name) {
                    return Err(#krate::codec::Error::Invalid(concat!(stringify!(#ident), ".", stringify!(#name), " out of range")));
                }
            });
        }
        encodes.push(enc);
        decodes.push(quote! {
            let #name: #ty = #dec;
            #(#checks)*
        });
        names.push(name);
    }

    let construct = match &data.fields {
        Fields::Named(_) => quote!(Self { #(#names),* }),
        Fields::Unnamed(_) => quote!(Self(#(#names),*)),
        Fields::Unit => quote!(Self),
    };

    let state = &attr.state;
    let dir = &attr.dir;
    let state_mod = format_ident!("{}", state.to_string().to_lowercase());
    let dir_mod = format_ident!("{}", dir.to_string().to_lowercase());
    let name = attr.name.value();
    let const_name = format_ident!("{}", name.replace('/', "_").to_uppercase());
    let cost = &attr.cost;
    let bucket = format_ident!("{}", to_camel(&attr.bucket.value()));
    let name_lit = &attr.name;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics #krate::codec::Encode for #ident #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn encode(&self, out: &mut ::std::vec::Vec<u8>) {
                #(#encodes)*
            }
        }

        impl #impl_generics #krate::codec::Decode for #ident #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn decode(r: &mut #krate::codec::Reader<'_>) -> #krate::codec::Result<Self> {
                #(#decodes)*
                Ok(#construct)
            }
        }

        impl #impl_generics #krate::Packet for #ident #ty_generics #where_clause {
            const ID: i32 = #krate::ids::#state_mod::#dir_mod::#const_name;
            const NAME: &'static str = #name_lit;
            const STATE: #krate::State = #krate::State::#state;
            const DIRECTION: #krate::Direction = #krate::Direction::#dir;
            const COST: u16 = #cost;
            const BUCKET: #krate::Bucket = #krate::Bucket::#bucket;
        }
    })
}
