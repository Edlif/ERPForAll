// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit report opt-in for data-callable methods. No report-library dependency.
use crate::rpc::{Effect, ExposureTable, Receiver};
use erp_fields::{Error, FieldType, Result};
use erp_registry::{ModelId, Registry};
use std::collections::{BTreeMap, BTreeSet};

/// A native addon's report helper declaration. Its method must be records-data-callable.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReportHelperDecl {
    addon: &'static str,
    model: &'static str,
    method: &'static str,
    dependencies: &'static [&'static str],
}
impl ReportHelperDecl {
    /// Declare every field path the batch method reads, relative to its receiver.
    pub const fn new(
        addon: &'static str,
        model: &'static str,
        method: &'static str,
        dependencies: &'static [&'static str],
    ) -> Self {
        Self {
            addon,
            model,
            method,
            dependencies,
        }
    }
}
/// Linked declarations; only installed addons contribute to a registry's catalog.
#[linkme::distributed_slice]
pub static REPORT_HELPERS: [ReportHelperDecl];

/// A helper resolved against one installed registry and its method table.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReportHelper {
    model: &'static str,
    method: &'static str,
    dependencies: Vec<&'static str>,
    parameters: Vec<&'static str>,
}
impl ReportHelper {
    /// Concrete model, including inherited registrations.
    pub fn model(&self) -> &'static str {
        self.model
    }
    /// Data-callable method name.
    pub fn method(&self) -> &'static str {
        self.method
    }
    /// Ordered binder parameter names used to canonicalize static calls.
    pub fn parameters(&self) -> &[&'static str] {
        &self.parameters
    }
    /// Effective dependency union of the installed declarations.
    pub fn dependencies(&self) -> &[&'static str] {
        &self.dependencies
    }
}

pub(crate) fn resolve(registry: &Registry, exposure: &ExposureTable) -> Result<Vec<ReportHelper>> {
    resolve_declarations(
        registry,
        exposure,
        &REPORT_HELPERS.iter().collect::<Vec<_>>(),
    )
}
fn resolve_declarations(
    registry: &Registry,
    exposure: &ExposureTable,
    linked: &[&ReportHelperDecl],
) -> Result<Vec<ReportHelper>> {
    let installed: BTreeSet<_> = registry.addons().iter().map(|a| a.name).collect();
    let declarations: Vec<_> = linked
        .iter()
        .copied()
        .filter(|d| installed.contains(d.addon))
        .collect();
    let mut seen = BTreeSet::new();
    for d in &declarations {
        if !seen.insert((d.addon, d.model, d.method))
            || !registry.models().iter().any(|model| {
                model
                    .mro
                    .iter()
                    .any(|m| m.name == d.model && m.addon == d.addon)
            })
        {
            return Err(Error::programming(format!(
                "report helper {}.{}: duplicate declaration or model not contributed by addon {}",
                d.model, d.method, d.addon
            )));
        }
    }
    let mut out = Vec::new();
    for model in registry.models() {
        let mut methods: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for d in &declarations {
            if model
                .mro
                .iter()
                .any(|m| m.name == d.model && m.addon == d.addon)
            {
                methods
                    .entry(d.method)
                    .or_default()
                    .extend(d.dependencies.iter().copied());
            }
        }
        for (method, dependencies) in methods {
            let fail = |why: &str| {
                Error::programming(format!("report helper {}.{method}: {why}", model.name))
            };
            if model.field(method).is_some() {
                return Err(fail("method name conflicts with a report field"));
            }
            let callable = exposure
                .data_callable(model.id, method)
                .ok_or_else(|| fail("method is not data-callable"))?;
            if callable.receiver() != Receiver::Records {
                return Err(fail("a model receiver is not a batch report helper"));
            }
            if callable
                .declared_effect()
                .is_some_and(|effect| effect != Effect::Read)
            {
                return Err(fail(
                    "a report helper cannot declare write or external effects",
                ));
            }
            for path in &dependencies {
                let mut current = Some(model);
                for segment in path.split('.') {
                    let spec = current.ok_or_else(|| {
                        fail(&format!("dependency {path} continues through a scalar"))
                    })?;
                    let field = spec.field(segment).ok_or_else(|| {
                        fail(&format!(
                            "dependency {path} has no field {}.{segment}",
                            spec.name
                        ))
                    })?;
                    current = match &field.ty {
                        FieldType::Many2one { comodel, .. }
                        | FieldType::One2many { comodel, .. }
                        | FieldType::Many2many { comodel, .. } => registry.model(comodel),
                        _ => None,
                    };
                }
            }
            out.push(ReportHelper {
                model: model.name,
                method,
                dependencies: dependencies.into_iter().collect(),
                parameters: callable.params().iter().map(|p| p.name()).collect(),
            });
        }
    }
    out.sort_by_key(|h| (h.model, h.method));
    Ok(out)
}

/// Exact records and fields a helper may read after the data stage has loaded them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReportRead {
    pub(crate) model: ModelId,
    pub(crate) fields: BTreeSet<erp_registry::FieldId>,
    pub(crate) ids: BTreeSet<i64>,
}
impl ReportRead {
    /// Resolve field names and require positive persisted ids. This restricts reads; it grants no access.
    pub fn new(registry: &Registry, model: &str, ids: &[i64], fields: &[&str]) -> Result<Self> {
        let spec = registry
            .model(model)
            .ok_or_else(|| Error::programming(format!("report helper: unknown model {model}")))?;
        if ids.iter().any(|id| *id <= 0) {
            return Err(Error::programming(
                "report helper: receiver ids must be positive",
            ));
        }
        let fields = fields
            .iter()
            .map(|name| {
                spec.field(name).map(|f| f.id).ok_or_else(|| {
                    Error::programming(format!("report helper: unknown field {model}.{name}"))
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            model: spec.id,
            fields,
            ids: ids.iter().copied().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "registration assertions"
    )]
    use super::*;
    use crate::{
        Dispatch, Records, Transaction,
        rpc::{BindError, Exposure, RpcArgs, RpcDecl, RpcImpl},
    };
    use erp_fields::{ErrorKind, Value};
    use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, RegistryBuilder};
    static BASE: AddonDescriptor = AddonDescriptor {
        name: "helper_base",
        depends: &[],
        exact_only: false,
    };
    static EXT: AddonDescriptor = AddonDescriptor {
        name: "helper_ext",
        depends: &["helper_base"],
        exact_only: false,
    };
    fn bind<'a>(
        _: &'a mut Transaction,
        _: Records,
        _: RpcArgs,
    ) -> crate::BoxFuture<'a, std::result::Result<Value, BindError>> {
        Box::pin(async { Ok(Value::list([])) })
    }
    static DATA: RpcImpl = RpcImpl {
        addon: "helper_base",
        model: "helper.doc",
        method: "label",
        decl: RpcDecl::DataCallable {
            receiver: Receiver::Records,
            params: &[],
            bind,
        },
        file: file!(),
        line: line!(),
    };
    static COLLISION: RpcImpl = RpcImpl {
        addon: "helper_base",
        model: "helper.doc",
        method: "name",
        decl: RpcDecl::DataCallable {
            receiver: Receiver::Records,
            params: &[],
            bind,
        },
        file: file!(),
        line: line!(),
    };
    static WRITE: RpcImpl = RpcImpl {
        addon: "helper_base",
        model: "helper.doc",
        method: "write_label",
        decl: RpcDecl::Expose {
            effect: Effect::Write,
            receiver: Receiver::Records,
            params: &[],
            bind,
        },
        file: file!(),
        line: line!(),
    };
    static MODEL: RpcImpl = RpcImpl {
        addon: "helper_base",
        model: "helper.doc",
        method: "model_label",
        decl: RpcDecl::DataCallable {
            receiver: Receiver::Model,
            params: &[],
            bind,
        },
        file: file!(),
        line: line!(),
    };
    static FIELDS: &[FieldDecl] = &[FieldDecl::char("name"), FieldDecl::char("secret")];
    static DOC: ModelDecl = ModelDecl::new("helper_base", "helper.doc").fields(FIELDS);
    static OVERRIDE: ModelDecl =
        ModelDecl::new("helper_ext", "helper.doc").inherit(&["helper.doc"]);
    static CHILD: ModelDecl = ModelDecl::new("helper_ext", "helper.child").inherit(&["helper.doc"]);
    fn registry() -> Registry {
        RegistryBuilder::new([&BASE, &EXT])
            .linked(false)
            .with_decls([&DOC, &OVERRIDE, &CHILD])
            .build()
            .unwrap()
    }
    #[test]
    fn report_helper_registration_filters_uninstalled_and_unions_inherited_dependencies() {
        let registry = registry();
        let dispatch = Dispatch::build(&registry, &[]).unwrap();
        let exposure = ExposureTable::build(&registry, &dispatch, &[&DATA]).unwrap();
        let base = ReportHelperDecl::new("helper_base", "helper.doc", "label", &["name"]);
        let ext = ReportHelperDecl::new("helper_ext", "helper.doc", "label", &["secret"]);
        let absent = ReportHelperDecl::new("not_installed", "absent", "missing", &[]);
        let result = resolve_declarations(&registry, &exposure, &[&base, &ext, &absent]).unwrap();
        assert_eq!(result.len(), 2);
        for model in ["helper.doc", "helper.child"] {
            let helper = result.iter().find(|h| h.model() == model).unwrap();
            assert_eq!(helper.dependencies(), &["name", "secret"]);
            assert!(matches!(
                exposure.get(registry.model_id(model).unwrap(), "label"),
                Some(Exposure::Internal)
            ));
        }
    }
    #[test]
    fn report_helper_registration_refuses_invalid_contracts_by_name() {
        let registry = registry();
        let dispatch = Dispatch::build(&registry, &[]).unwrap();
        let exposure =
            ExposureTable::build(&registry, &dispatch, &[&DATA, &WRITE, &MODEL, &COLLISION])
                .unwrap();
        for (method, deps, reason) in [
            ("name", &[][..], "conflicts with a report field"),
            ("missing", &[][..], "not data-callable"),
            ("write_label", &[][..], "write or external"),
            ("model_label", &[][..], "model receiver"),
            ("label", &["missing"][..], "no field"),
            ("label", &["name.nope"][..], "scalar"),
        ] {
            let declaration = ReportHelperDecl::new("helper_base", "helper.doc", method, deps);
            let error = resolve_declarations(&registry, &exposure, &[&declaration]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Programming);
            assert!(
                error.message().contains(method) && error.message().contains(reason),
                "{error}"
            );
        }
        let d = ReportHelperDecl::new("helper_base", "helper.doc", "label", &["name"]);
        let error = resolve_declarations(&registry, &exposure, &[&d, &d]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Programming);
        assert!(error.message().contains("duplicate") && error.message().contains("label"));
        let d = ReportHelperDecl::new("helper_ext", "no.model", "label", &[]);
        let error = resolve_declarations(&registry, &exposure, &[&d]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Programming);
        assert!(
            error.message().contains("not contributed")
                && error.message().contains("no.model.label")
        );
    }
}
