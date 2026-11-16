// SPDX-License-Identifier: MIT OR Apache-2.0
//! The image limits of a database as declared technical parameters (B9 rows S2, S5 and
//! S7, placement 4): the most pixels and bytes an image field accepts, the quality of a
//! JPEG the server encodes, and how long a write waits for a decode. The ORM reads them from
//! `ir.config_parameter` when it admits a picture (`erp_sdk::image`,
//! `Transaction::ensure_image_policy`); these declarations add the label, the default row and the
//! write-time check with the same default, floor and hard bound ([`ImagePolicy::PARAMETERS`]).
use erp_sdk::Result;
use erp_sdk::image::ImagePolicy;
use erp_sdk::settings::ParameterDecl;

/// The image parameters, with the bounds of [`ImagePolicy`]. Listed by
/// `crate::system_parameters::declarations`, base's one parameter factory.
pub(crate) fn declarations() -> Result<Vec<ParameterDecl>> {
    let int = |value: u64| {
        i64::try_from(value)
            .map_err(|_| erp_sdk::Error::programming("image parameter bound overflows i64"))
    };
    ImagePolicy::PARAMETERS
        .iter()
        .map(|p| {
            ParameterDecl::integer(p.key, p.label, int(p.default)?, (int(p.min)?, int(p.max)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The declared parameters accept and refuse the same values as the ORM's image policy at both
    // bounds, so a value the Technical form accepts is never refused when a picture is written.
    #[test]
    fn declarations_match_the_image_policy() {
        let declared = declarations().unwrap();
        assert_eq!(
            declared.iter().map(ParameterDecl::key).collect::<Vec<_>>(),
            [
                "base.image_max_pixels",
                "base.image_max_bytes",
                "base.image_jpeg_quality",
                "base.image_decode_timeout_secs"
            ]
        );
        for decl in &declared {
            let key = decl.key();
            let policy = |text: &str| ImagePolicy::from_parameters(|k| (k == key).then_some(text));
            assert!(policy(decl.default_text()).is_ok(), "{key} default");
            let erp_sdk::settings::SettingKind::Integer { min, max } = decl.kind() else {
                panic!("{key} is not an integer parameter");
            };
            for ok in [*min, *max] {
                assert!(decl.check_text(Some(&ok.to_string())).is_ok(), "{key} {ok}");
                assert!(policy(&ok.to_string()).is_ok(), "{key} {ok}");
            }
            for refused in [min - 1, max + 1] {
                assert!(
                    decl.check_text(Some(&refused.to_string())).is_err(),
                    "{key} {refused}"
                );
                assert!(policy(&refused.to_string()).is_err(), "{key} {refused}");
            }
        }
        let default = ImagePolicy::from_parameters(|_| None).unwrap();
        assert_eq!(default, ImagePolicy::default());
    }
}
