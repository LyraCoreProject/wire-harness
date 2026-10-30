use syn::visit::{self, Visit};

const MODULES: &[&str] = &[
    "catalogue",
    "compare",
    "decode",
    "scenario",
    "trace",
    "values_mask",
    "spatial",
    "variants",
];

#[derive(Default)]
struct Boundary {
    forbidden: Vec<String>,
}

impl Boundary {
    fn check(&mut self, path: &[String]) {
        if path
            .iter()
            .any(|part| part.starts_with("wow_") || part.starts_with("lyracore") || part == "super")
            || (path.first().is_some_and(|part| part == "crate")
                && !path
                    .get(1)
                    .is_some_and(|part| MODULES.contains(&part.as_str())))
        {
            self.forbidden.push(path.join("::"));
        }
    }

    fn imports(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.imports(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.imports(prefix, item);
                }
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.check(prefix);
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.check(prefix);
                prefix.pop();
            }
            syn::UseTree::Glob(_) => self.check(prefix),
        }
    }
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test")))
    })
}

impl<'ast> Visit<'ast> for Boundary {
    fn visit_visibility(&mut self, _: &'ast syn::Visibility) {}

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !test_only(&item.attrs) {
            assert!(
                item.content.is_some(),
                "comparison submodules must join the boundary check"
            );
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !test_only(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !test_only(&item.attrs) {
            self.imports(&mut Vec::new(), &item.tree);
        }
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if !test_only(&item.attrs) {
            self.check(&[item.ident.to_string()]);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.check(
            &path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>(),
        );
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, item: &'ast syn::Macro) {
        assert!(
            !item.path.is_ident("include"),
            "comparison code must be scanned, not included from another file"
        );
        visit::visit_macro(self, item);
    }
}

fn forbidden(source: &str) -> Vec<String> {
    let mut boundary = Boundary::default();
    boundary.visit_file(&syn::parse_file(source).unwrap());
    boundary.forbidden
}

#[test]
fn decoders_and_comparison_dependencies_never_import_shared_protocol_or_server_code() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for module in MODULES {
        let source = std::fs::read_to_string(root.join(format!("{module}.rs"))).unwrap();
        assert!(
            forbidden(&source).is_empty(),
            "{module}: {:?}",
            forbidden(&source)
        );
    }
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    for (alias, dependency) in manifest["dependencies"].as_table().unwrap() {
        let package = dependency
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(alias);
        assert!(
            !package.starts_with("lyracore"),
            "server dependency {package}"
        );
        assert!(
            !package.starts_with("wow_") || package == alias,
            "protocol crate alias must be scanned: {alias}"
        );
    }
}

#[test]
fn boundary_check_detects_imports_qualified_calls_and_indirect_decoder_access() {
    for mutation in [
        "use wow_world_messages::vanilla::SMSG_PONG;",
        "use wow_srp as codec;",
        "extern crate wow_world_base as codec;",
        "fn decode() { lyracore_protocol::decode(); }",
        "use crate::{decode, WireClient};",
        "use super::WireClient;",
        "fn decode() { crate::cli::parse_race(\"human\"); }",
    ] {
        assert!(!forbidden(mutation).is_empty(), "missed {mutation}");
    }
    assert!(forbidden("// use wow_world_messages::vanilla::SMSG_PONG;\n#[cfg(test)] mod tests { use wow_world_messages::vanilla::SMSG_PONG; }\nfn name() -> &'static str { \"lyracore_protocol::decode\" }").is_empty());
}
