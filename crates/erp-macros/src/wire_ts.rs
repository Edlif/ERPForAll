// SPDX-License-Identifier: MIT OR Apache-2.0

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, Lit, Meta, Token, Type, parse_quote,
    punctuated::Punctuated,
};

/// Serde has already derived its read and write implementations on entry. ts-rs needs the
/// serialization shape, including renames, tags, flattening and optional fields.
pub(crate) fn expand(mut item: DeriveInput) -> syn::Result<TokenStream> {
    for attr in item
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
    {
        let derives = attr.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)?;
        if derives.iter().any(|path| {
            path.segments
                .last()
                .is_some_and(|s| s.ident == "Deserialize" || s.ident == "Serialize")
        }) {
            return Err(syn::Error::new_spanned(
                attr,
                "apply wire_ts after Serde derives",
            ));
        }
    }
    let newtype = match &item.data {
        Data::Struct(data) => match &data.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => Some(&fields.unnamed[0].ty),
            _ => None,
        },
        _ => None,
    };
    project(&mut item.attrs, |meta| {
        if meta.path().is_ident("try_from") || meta.path().is_ident("deny_unknown_fields") {
            return Ok(false);
        }
        if meta.path().is_ident("into") {
            let Expr::Lit(value) = &meta.require_name_value()?.value else {
                return Err(syn::Error::new_spanned(meta, "expected a Serde wire type"));
            };
            if let Lit::Str(literal) = &value.lit {
                let wire_type: Type = literal.parse()?;
                if newtype == Some(&wire_type) {
                    return Ok(false);
                }
            }
            return Err(syn::Error::new_spanned(
                meta,
                "the schema must describe the serialized type of this conversion",
            ));
        }
        Ok(true)
    })?;
    let field_shape = |fields: &mut Fields| -> syn::Result<()> {
        for field in fields {
            project(&mut field.attrs, |meta| {
                Ok(!meta.path().is_ident("deserialize_with"))
            })?;
        }
        Ok(())
    };
    match &mut item.data {
        Data::Struct(data) => field_shape(&mut data.fields)?,
        Data::Enum(data) => {
            for variant in &mut data.variants {
                field_shape(&mut variant.fields)?;
            }
        }
        Data::Union(_) => return Err(syn::Error::new_spanned(item, "wire unions are unsupported")),
    }
    Ok(quote!(#[derive(ts_rs::TS)] #item))
}

fn project(
    attrs: &mut Vec<Attribute>,
    keep: impl Fn(&Meta) -> syn::Result<bool>,
) -> syn::Result<()> {
    let mut projected = Vec::with_capacity(attrs.len());
    for attr in attrs.drain(..) {
        if !attr.path().is_ident("serde") {
            projected.push(attr);
            continue;
        }
        let options = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        let mut retained = Vec::new();
        for option in options {
            if keep(&option)? {
                retained.push(option);
            }
        }
        if !retained.is_empty() {
            projected.push(parse_quote!(#[serde(#(#retained),*)]));
        }
    }
    *attrs = projected;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_remove_wire_validation_before_serde_uses_it() {
        let input = parse_quote! {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Record { id: u32 }
        };
        assert!(
            expand(input)
                .unwrap_err()
                .to_string()
                .contains("after Serde derives")
        );
    }

    #[test]
    fn refuses_to_infer_a_changed_serialization_shape_from_rust_storage() {
        let input = parse_quote! {
            #[serde(into = "String")]
            struct Id(u64);
        };
        assert!(
            expand(input)
                .unwrap_err()
                .to_string()
                .contains("serialized type")
        );
    }
}
