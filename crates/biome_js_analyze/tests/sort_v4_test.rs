use biome_css_parser::{CssParserOptions, parse_css};
use biome_languages::CssFileSource;
use biome_tailwind_logic::sorted_classes::{
    TailwindRegistry, extract_tailwind_registry, sort_class_list,
};
use biome_tailwind_parser::parse_tailwind;
use biome_test_utils::scripts_from_json;

#[test]
fn fixtures() {
    let registry = TailwindRegistry::default();
    insta::glob!("sort_v4/*.jsonc", |path| {
        let raw = std::fs::read_to_string(path).expect("read fixture");
        let cases = scripts_from_json("jsonc", &raw).expect("parse jsonc array");
        let rendered = cases
            .iter()
            .map(|input| {
                let parsed = parse_tailwind(input);
                let sorted = sort_class_list(&parsed.tree(), &registry);
                format!("input:  {input}\nsorted: {sorted}")
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        let snap_name = path.file_stem().unwrap().to_str().unwrap();
        insta::with_settings!(
            {
                snapshot_path => path.parent().unwrap(),
                prepend_module_to_snapshot => false,
            },
            { insta::assert_snapshot!(snap_name, rendered) }
        );
    });
}

/// Every `stylesheet/<name>.jsonc` sorts its cases with the registry
/// extracted from the sibling `stylesheet/<name>.css`. Expectations were
/// produced by `packages/tailwindcss-config-analyzer/src/v4/oracle-with-css.ts`
/// (Tailwind 4.2.4, `getClassOrder` sorted the way
/// `prettier-plugin-tailwindcss` sorts) and the snapshot must stay
/// byte-identical to its output.
#[test]
fn stylesheet_fixtures() {
    insta::glob!("sort_v4/stylesheet/*.jsonc", |path| {
        let css_path = path.with_extension("css");
        let css = std::fs::read_to_string(&css_path).expect("read stylesheet fixture");
        let options = CssParserOptions::default().allow_tailwind_directives();
        let root = parse_css(&css, CssFileSource::css(), options).tree();
        let mut registry = TailwindRegistry::new();
        extract_tailwind_registry(&root, &mut registry);

        let raw = std::fs::read_to_string(path).expect("read fixture");
        let cases = scripts_from_json("jsonc", &raw).expect("parse jsonc array");
        let rendered = cases
            .iter()
            .map(|input| {
                let parsed = parse_tailwind(input);
                let sorted = sort_class_list(&parsed.tree(), &registry);
                format!("input:  {input}\nsorted: {sorted}")
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        let snap_name = path.file_stem().unwrap().to_str().unwrap();
        insta::with_settings!(
            {
                snapshot_path => path.parent().unwrap(),
                prepend_module_to_snapshot => false,
            },
            { insta::assert_snapshot!(snap_name, rendered) }
        );
    });
}
