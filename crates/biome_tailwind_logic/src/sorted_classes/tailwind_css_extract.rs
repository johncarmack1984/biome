//! Lowers a Tailwind stylesheet's `@utility`, `@custom-variant`, and
//! `@theme` at-rules into a [TailwindRegistry].
//!
//! Only what Tailwind's sorter derives from those directives is kept:
//! which names exist, which theme keys they accept, and each utility's
//! `propertySort` (the property-order positions its declarations set,
//! plus the declaration count). Directives Biome cannot follow —
//! `@plugin`, `@config`, `@import "tailwindcss" prefix(…)` — are left
//! alone; classes they would add stay unknown to the sorter.

use biome_css_syntax::{
    AnyCssAtRule, AnyCssDeclarationName, AnyCssDeclarationOrRule, AnyCssDeclarationOrRuleBlock,
    AnyCssRoot, AnyCssRule, AnyCssRuleBlock, AnyCssRuleListItem, AnyTwCustomVariantSelector,
    AnyTwCustomVariantShorthand, CssDeclaration, CssDeclarationOrRuleList, CssFunction,
    CssGenericProperty, TwApplyAtRule, TwCustomVariantAtRule, TwThemeAtRule, TwUtilityAtRule,
};
use biome_rowan::{AstNode, AstNodeList, SyntaxNodeCast, TokenText, WalkEvent};
use biome_tailwind_parser::parse_tailwind;
use std::collections::VecDeque;

use super::sort_v4::utility_placement;
use super::tailwind_preset_v4::PROPERTY_INDEX;
use super::tailwind_preset_v4_types::CssDataType;
use super::tailwind_registry::{BareValueType, FunctionalDecl, TailwindRegistry, ValueShape};

/// Register everything `root` declares into `registry`, in document
/// order, so a later `--color-*: initial` clears an earlier `--color-x`
/// and an `@apply` inside `@utility` sees utilities defined above it.
pub fn extract_tailwind_registry(root: &AnyCssRoot, registry: &mut TailwindRegistry) {
    for event in root.syntax().preorder() {
        let WalkEvent::Enter(node) = event else {
            continue;
        };
        if let Some(rule) = TwUtilityAtRule::cast_ref(&node) {
            visit_utility(&rule, registry);
        } else if let Some(rule) = TwCustomVariantAtRule::cast_ref(&node) {
            visit_custom_variant(&rule, registry);
        } else if let Some(rule) = TwThemeAtRule::cast_ref(&node) {
            visit_theme(&rule, registry);
        }
    }
}

fn visit_utility(rule: &TwUtilityAtRule, registry: &mut TailwindRegistry) {
    let Ok(name) = rule.name() else { return };
    let block = rule.block().ok();
    let Some(items) = block
        .as_ref()
        .and_then(AnyCssDeclarationOrRuleBlock::as_css_declaration_or_rule_block)
        .map(|block| block.items())
    else {
        return;
    };

    if let Some(ident) = name.as_css_identifier() {
        let Ok(token) = ident.value_token() else {
            return;
        };
        let (indices, count) = static_property_sort(&items, registry);
        registry.register_static_utility(token.text_trimmed(), &indices, count);
    } else if let Some(functional) = name.as_tw_functional_utility_name() {
        let Ok(token) = functional
            .identifier()
            .and_then(|ident| ident.value_token())
        else {
            return;
        };
        let decls = functional_declarations(&items);
        registry.register_functional_utility(token.text_trimmed(), decls);
    }
}

fn visit_custom_variant(rule: &TwCustomVariantAtRule, registry: &mut TailwindRegistry) {
    let Ok(token) = rule.name().and_then(|name| name.value_token()) else {
        return;
    };
    // Tailwind's `Compounds` flags for what the variant emits: `1` for
    // at-rules (`@media …`), `2` for style rules (selectors).
    let compounds = match rule.selector() {
        Ok(AnyTwCustomVariantSelector::TwCustomVariantShorthand(shorthand)) => {
            match shorthand.selector() {
                Ok(AnyTwCustomVariantShorthand::CssAtRuleDeclarator(_)) => 1,
                _ => 2,
            }
        }
        Ok(AnyTwCustomVariantSelector::AnyCssRuleBlock(AnyCssRuleBlock::CssRuleBlock(block))) => {
            let only_at_rules = block.rules().iter().all(|rule| {
                matches!(
                    rule,
                    AnyCssRuleListItem::AnyCssRule(AnyCssRule::CssAtRule(_))
                )
            });
            if only_at_rules { 1 } else { 2 }
        }
        _ => 2,
    };
    registry.register_custom_variant(token.text_trimmed(), compounds);
}

fn visit_theme(rule: &TwThemeAtRule, registry: &mut TailwindRegistry) {
    let Ok(block) = rule.block() else { return };
    let Some(block) = block.as_css_declaration_or_rule_block() else {
        return;
    };
    for item in block.items() {
        let AnyCssDeclarationOrRule::CssDeclarationWithSemicolon(item) = item else {
            continue;
        };
        let Some(property) = item
            .declaration()
            .ok()
            .and_then(|declaration| declaration.property().ok())
            .and_then(|property| property.as_css_generic_property().cloned())
        else {
            continue;
        };
        let Ok(name) = property.name() else { continue };
        match name {
            // `--color-*: initial` / `--*: initial` reset a namespace.
            AnyCssDeclarationName::TwValueThemeReference(reference) => {
                let Ok(token) = reference.reference().and_then(|ident| ident.value_token()) else {
                    continue;
                };
                let prefix = token.text_trimmed();
                if prefix == "--" {
                    registry.clear_all_theme_namespaces();
                } else {
                    let mut buf = [0u8; 64];
                    if let Some(prefix) = super::tailwind_registry::join_into(&mut buf, prefix, "-")
                    {
                        registry.clear_theme_namespace(prefix);
                    }
                }
            }
            AnyCssDeclarationName::AnyCssDashedIdentifier(dashed) => {
                let Some(token) = dashed
                    .as_css_dashed_identifier()
                    .and_then(|ident| ident.value_token().ok())
                else {
                    continue;
                };
                let name = token.text_trimmed();
                if name == "--*" {
                    registry.clear_all_theme_namespaces();
                    continue;
                }
                let value = property_value_text(&property);
                registry.register_theme_key(name, value.as_deref().unwrap_or(""));
            }
            _ => {}
        }
    }
}

/// The value of a theme declaration when it is a single component
/// (`120rem`, `#abc`, `display`); breakpoint and container comparison
/// need the length text, `--tw-sort` names a property.
fn property_value_text(property: &CssGenericProperty) -> Option<String> {
    let value = property.value().ok()?;
    let list = value.as_css_generic_component_value_list()?;
    if list.len() != 1 {
        return None;
    }
    Some(list.iter().next()?.syntax().text_trimmed().to_string())
}

/// Tailwind's `getPropertySort` for a static `@utility` block: walk the
/// declarations breadth-first, collect the property-order position of
/// each ordered property until a `--tw-sort:` hint, and count every
/// declaration. `@apply` expands to the applied utilities' declarations,
/// looked up in the registry as built so far.
fn static_property_sort(
    items: &CssDeclarationOrRuleList,
    registry: &TailwindRegistry,
) -> (Vec<u16>, u8) {
    let mut indices: Vec<u16> = Vec::new();
    let mut count: usize = 0;
    let mut hinted = false;

    let mut queue: VecDeque<AnyCssDeclarationOrRule> = items.iter().collect();
    while let Some(item) = queue.pop_front() {
        match item {
            AnyCssDeclarationOrRule::CssDeclarationWithSemicolon(item) => {
                let Ok(declaration) = item.declaration() else {
                    continue;
                };
                count += 1;
                if hinted {
                    continue;
                }
                let Some((name, property)) = generic_property(&declaration) else {
                    continue;
                };
                if name.text() == "--tw-sort" {
                    if let Some(index) = property_value_text(&property)
                        .and_then(|value| PROPERTY_INDEX.get(value.as_str()).copied())
                    {
                        indices.push(index);
                        hinted = true;
                    }
                    continue;
                }
                if let Some(index) = PROPERTY_INDEX.get(name.text()).copied() {
                    indices.push(index);
                }
            }
            AnyCssDeclarationOrRule::AnyCssRule(AnyCssRule::CssAtRule(at_rule)) => {
                match at_rule.rule() {
                    Ok(AnyCssAtRule::TwApplyAtRule(apply)) => {
                        let (applied, applied_count) = apply_property_sort(&apply, registry);
                        if !hinted {
                            indices.extend(applied);
                        }
                        count += applied_count;
                    }
                    Ok(other) => queue.extend(nested_items(other.syntax())),
                    Err(_) => {}
                }
            }
            AnyCssDeclarationOrRule::AnyCssRule(rule) => {
                queue.extend(nested_items(rule.syntax()));
            }
            _ => {}
        }
    }

    indices.sort_unstable();
    indices.dedup();
    (indices, u8::try_from(count).unwrap_or(u8::MAX))
}

/// The declarations directly inside a nested rule or at-rule block.
fn nested_items(
    rule: &biome_css_syntax::CssSyntaxNode,
) -> impl Iterator<Item = AnyCssDeclarationOrRule> {
    rule.children()
        .find_map(|child| child.cast::<biome_css_syntax::CssDeclarationOrRuleBlock>())
        .into_iter()
        .flat_map(|block| block.items().iter())
}

/// The property positions and declaration count `@apply a b c`
/// contributes: each applied class is classified like a candidate in a
/// class attribute; classes the sorter cannot place contribute nothing,
/// as Tailwind would fail to compile them.
fn apply_property_sort(apply: &TwApplyAtRule, registry: &TailwindRegistry) -> (Vec<u16>, usize) {
    let mut indices = Vec::new();
    let mut count = 0usize;
    for class in apply.classes() {
        let Ok(token) = class.value_token() else {
            continue;
        };
        let parsed = parse_tailwind(token.text_trimmed());
        for candidate in parsed.tree().candidates() {
            if let Some((signature, applied_count)) = utility_placement(&candidate, registry) {
                indices.extend_from_slice(signature.as_slice());
                count += usize::from(applied_count);
            }
        }
    }
    (indices, count)
}

/// Every declaration of a functional `@utility name-* { … }` block,
/// breadth-first like [static_property_sort], with the `--value(…)` and
/// `--modifier(…)` shapes each one resolves through.
fn functional_declarations(items: &CssDeclarationOrRuleList) -> Vec<FunctionalDecl> {
    let mut decls = Vec::new();
    let mut queue: VecDeque<AnyCssDeclarationOrRule> = items.iter().collect();
    while let Some(item) = queue.pop_front() {
        match item {
            AnyCssDeclarationOrRule::CssDeclarationWithSemicolon(item) => {
                let Ok(declaration) = item.declaration() else {
                    continue;
                };
                let Some((name, property)) = generic_property(&declaration) else {
                    decls.push(FunctionalDecl {
                        property: None,
                        sort_hint: None,
                        value: None,
                        modifier: None,
                    });
                    continue;
                };
                let sort_hint = (name.text() == "--tw-sort")
                    .then(|| property_value_text(&property))
                    .flatten()
                    .and_then(|value| PROPERTY_INDEX.get(value.as_str()).copied());
                let mut value = None;
                let mut modifier = None;
                let functions = property
                    .value()
                    .ok()
                    .into_iter()
                    .flat_map(|value| value.syntax().descendants())
                    .filter_map(CssFunction::cast);
                for function in functions {
                    let Some(function_name) = function
                        .name()
                        .ok()
                        .and_then(|name| name.as_css_identifier().cloned())
                        .and_then(|ident| ident.value_token().ok())
                    else {
                        continue;
                    };
                    match function_name.text_trimmed() {
                        "--value" => value
                            .get_or_insert_with(Vec::new)
                            .extend(value_shapes(&function)),
                        "--modifier" => modifier
                            .get_or_insert_with(Vec::new)
                            .extend(value_shapes(&function)),
                        _ => {}
                    }
                }
                decls.push(FunctionalDecl {
                    property: PROPERTY_INDEX.get(name.text()).copied(),
                    sort_hint,
                    value: value.map(Vec::into_boxed_slice),
                    modifier: modifier.map(Vec::into_boxed_slice),
                });
            }
            AnyCssDeclarationOrRule::AnyCssRule(rule) => {
                queue.extend(nested_items(rule.syntax()));
            }
            _ => {}
        }
    }
    decls
}

/// The argument shapes of one `--value(…)` / `--modifier(…)` call.
fn value_shapes(function: &CssFunction) -> Vec<ValueShape> {
    let mut shapes = Vec::new();
    for node in function.items().syntax().descendants() {
        if let Some(reference) = node
            .clone()
            .cast::<biome_css_syntax::TwValueThemeReference>()
        {
            // `--color-*` (and `--color`, which Tailwind normalizes to it).
            if let Ok(token) = reference.reference().and_then(|ident| ident.value_token()) {
                shapes.push(ValueShape::Theme(theme_prefix(token.text_trimmed())));
            }
        } else if let Some(dashed) = node.clone().cast::<biome_css_syntax::CssDashedIdentifier>() {
            if dashed.syntax().parent().is_some_and(|parent| {
                biome_css_syntax::TwValueThemeReference::can_cast(parent.kind())
            }) {
                continue;
            }
            if let Ok(token) = dashed.value_token() {
                shapes.push(ValueShape::Theme(theme_prefix(token.text_trimmed())));
            }
        } else if let Some(bracketed) = node.clone().cast::<biome_css_syntax::CssBracketedValue>() {
            let data_type = bracketed
                .items()
                .iter()
                .next()
                .and_then(|item| item.syntax().first_token())
                .and_then(|token| css_data_type(token.text_trimmed()));
            shapes.push(ValueShape::Arbitrary(data_type));
        } else if let Some(string) = node.clone().cast::<biome_css_syntax::CssString>() {
            if let Ok(literal) = string.inner_string_text() {
                shapes.push(ValueShape::Literal(literal.text().into()));
            }
        } else if let Some(ident) = node.cast::<biome_css_syntax::CssIdentifier>() {
            if ident.syntax().parent().is_some_and(|parent| {
                biome_css_syntax::CssBracketedValue::can_cast(parent.kind())
                    || biome_css_syntax::CssFunction::can_cast(parent.kind())
            }) {
                continue;
            }
            if let Some(bare) = ident
                .value_token()
                .ok()
                .and_then(|token| BareValueType::from_keyword(token.text_trimmed()))
            {
                shapes.push(ValueShape::Bare(bare));
            }
        }
    }
    shapes
}

/// `--color` → `--color-`: the prefix a key is joined to.
fn theme_prefix(reference: &str) -> Box<str> {
    let mut prefix = String::with_capacity(reference.len() + 1);
    prefix.push_str(reference.trim_end_matches("-*"));
    prefix.push('-');
    prefix.into_boxed_str()
}

/// The `[type]` a `--value([type])` argument names; `[*]` and unknown
/// names are `None` (any arbitrary value).
fn css_data_type(name: &str) -> Option<CssDataType> {
    Some(match name {
        "color" => CssDataType::Color,
        "length" => CssDataType::Length,
        "percentage" => CssDataType::Percentage,
        "number" => CssDataType::Number,
        "integer" => CssDataType::Integer,
        "ratio" => CssDataType::Ratio,
        "angle" => CssDataType::Angle,
        "url" => CssDataType::Url,
        "position" => CssDataType::Position,
        "bg-size" => CssDataType::BgSize,
        "line-width" => CssDataType::LineWidth,
        "image" => CssDataType::Image,
        "absolute-size" => CssDataType::AbsoluteSize,
        "relative-size" => CssDataType::RelativeSize,
        "vector" => CssDataType::Vector,
        _ => return None,
    })
}

/// A declaration's property name and generic property node; `None` for
/// non-generic (SCSS, bogus) declarations.
fn generic_property(declaration: &CssDeclaration) -> Option<(TokenText, CssGenericProperty)> {
    let property = declaration.property().ok()?;
    let property = property.as_css_generic_property()?.clone();
    let name = match property.name().ok()? {
        AnyCssDeclarationName::CssIdentifier(ident) => ident.value_token().ok()?,
        AnyCssDeclarationName::AnyCssDashedIdentifier(dashed) => {
            dashed.as_css_dashed_identifier()?.value_token().ok()?
        }
        _ => return None,
    };
    Some((name.token_text_trimmed(), property))
}

#[cfg(test)]
mod tests {
    use super::super::sort_v4::sort_class_list;
    use super::super::tailwind_preset_v4_types::ThemeNamespace;
    use super::*;
    use biome_css_parser::{CssParserOptions, parse_css};
    use biome_languages::CssFileSource;

    fn registry_from(source: &str) -> TailwindRegistry {
        let options = CssParserOptions::default().allow_tailwind_directives();
        let root = parse_css(source, CssFileSource::css(), options).tree();
        let mut registry = TailwindRegistry::new();
        extract_tailwind_registry(&root, &mut registry);
        registry
    }

    fn sort(input: &str, registry: &TailwindRegistry) -> String {
        sort_class_list(&parse_tailwind(input).tree(), registry)
    }

    #[test]
    fn static_utility_signature_and_count() {
        let reg = registry_from(
            "@utility card { --card-pad: 1rem; padding: var(--card-pad); background-color: white; padding: 2rem; }",
        );
        let entry = reg.custom_static("card").expect("card registered");
        // Every declaration counts, including the custom property and
        // the duplicate; only ordered properties contribute positions.
        assert_eq!(entry.count, 4);
        let mut expected = [
            *PROPERTY_INDEX.get("padding").unwrap(),
            *PROPERTY_INDEX.get("background-color").unwrap(),
        ];
        expected.sort_unstable();
        assert_eq!(entry.sig.as_ref(), &expected[..]);
    }

    #[test]
    fn tw_sort_hint_replaces_later_positions() {
        let reg = registry_from("@utility grid-thing { --tw-sort: display; padding: 1rem; }");
        let entry = reg
            .custom_static("grid-thing")
            .expect("grid-thing registered");
        assert_eq!(entry.count, 2);
        assert_eq!(
            entry.sig.as_ref(),
            &[*PROPERTY_INDEX.get("display").unwrap()]
        );
    }

    #[test]
    fn nested_rule_declarations_count() {
        let reg = registry_from(
            "@utility hoverable { color: red; &:hover { color: blue; opacity: 0.5; } }",
        );
        let entry = reg
            .custom_static("hoverable")
            .expect("hoverable registered");
        assert_eq!(entry.count, 3);
        assert_eq!(entry.sig.len(), 2);
    }

    #[test]
    fn apply_folds_the_applied_utilities() {
        let reg = registry_from("@utility btn { @apply px-4 py-2 rounded; cursor: pointer; }");
        let entry = reg.custom_static("btn").expect("btn registered");
        // px-4 (padding-inline) + py-2 (padding-block) + rounded + cursor
        assert_eq!(entry.count, 4);
        assert!(
            entry
                .sig
                .contains(PROPERTY_INDEX.get("padding-inline").unwrap())
        );
        assert!(
            entry
                .sig
                .contains(PROPERTY_INDEX.get("border-radius").unwrap())
        );
        assert!(entry.sig.contains(PROPERTY_INDEX.get("cursor").unwrap()));
    }

    #[test]
    fn functional_utility_shapes() {
        let reg = registry_from(
            r#"@utility tab-* {
                tab-size: --value(--tab-size-*, integer, [integer], "inherit");
                line-height: --modifier(--leading-*, [length]);
            }"#,
        );
        let entry = reg.custom_functional("tab").expect("tab-* registered");
        assert_eq!(entry.decls.len(), 2);
        let tab_size = &entry.decls[0];
        assert_eq!(tab_size.property, PROPERTY_INDEX.get("tab-size").copied());
        assert_eq!(
            tab_size.value.as_deref(),
            Some(
                &[
                    ValueShape::Theme("--tab-size-".into()),
                    ValueShape::Bare(BareValueType::Integer),
                    ValueShape::Arbitrary(Some(CssDataType::Integer)),
                    ValueShape::Literal("inherit".into()),
                ][..]
            )
        );
        assert!(tab_size.modifier.is_none());
        let line_height = &entry.decls[1];
        assert!(line_height.value.is_none());
        assert_eq!(
            line_height.modifier.as_deref(),
            Some(
                &[
                    ValueShape::Theme("--leading-".into()),
                    ValueShape::Arbitrary(Some(CssDataType::Length)),
                ][..]
            )
        );
    }

    #[test]
    fn functional_value_inside_spacing_function() {
        let reg = registry_from("@utility gap-x-* { column-gap: --spacing(--value(number)); }");
        let entry = reg.custom_functional("gap-x").unwrap();
        assert_eq!(
            entry.decls[0].value.as_deref(),
            Some(&[ValueShape::Bare(BareValueType::Number)][..])
        );
    }

    #[test]
    fn custom_variant_shapes() {
        let reg = registry_from(
            r#"
            @custom-variant dark (&:is(.dark *));
            @custom-variant hocus (&:hover, &:focus);
            @custom-variant any-hover (@media (any-hover: hover));
            @custom-variant theme-midnight { &:where([data-theme="midnight"] *) { @slot; } }
            "#,
        );
        assert!(reg.get_variant("dark").is_some());
        assert_eq!(reg.get_variant_entry("hocus").unwrap().compounds, 2);
        assert_eq!(reg.get_variant_entry("any-hover").unwrap().compounds, 1);
        assert_eq!(
            reg.get_variant_entry("theme-midnight").unwrap().compounds,
            2
        );
    }

    #[test]
    fn theme_keys_breakpoints_and_resets() {
        let reg = registry_from(
            r#"@theme {
                --color-brand: #abc;
                --breakpoint-3xl: 120rem;
                --container-8xl: 96rem;
                --tab-size-github: 8;
                --text-xs--line-height: 1rem;
            }"#,
        );
        assert!(reg.theme_contains(ThemeNamespace::Color, "brand"));
        assert_eq!(reg.get_breakpoint_value("3xl"), Some("120rem"));
        assert_eq!(reg.get_container_value("8xl"), Some("96rem"));
        assert!(reg.get_variant("3xl").is_some());
        assert!(reg.theme_var_contains("--tab-size-", "github"));
        assert!(!reg.theme_contains(ThemeNamespace::Text, "xs--line-height"));

        let reg = registry_from("@theme { --color-*: initial; --color-brand: #abc; }");
        assert!(!reg.theme_contains(ThemeNamespace::Color, "red-500"));
        assert!(reg.theme_contains(ThemeNamespace::Color, "brand"));

        let reg = registry_from("@theme { --*: initial; }");
        assert!(!reg.theme_contains(ThemeNamespace::Color, "red-500"));
        assert!(!reg.theme_contains(ThemeNamespace::Text, "lg"));
    }

    #[test]
    fn theme_inline_and_static_forms() {
        let reg = registry_from(
            "@theme inline { --color-brand: #abc; } @theme static { --color-mint: #0f0; }",
        );
        assert!(reg.theme_contains(ThemeNamespace::Color, "brand"));
        assert!(reg.theme_contains(ThemeNamespace::Color, "mint"));
    }

    #[test]
    fn extracted_registry_changes_the_sort() {
        let reg = registry_from(
            r#"
            @custom-variant dark (&:is(.dark *));
            @theme { --color-brand: #3b82f6; --breakpoint-3xl: 120rem; }
            @utility center-flex { display: flex; align-items: center; justify-content: center; }
            "#,
        );
        // `bg-brand` is a color, `center-flex` sorts by its properties,
        // `3xl:` is a breakpoint after `2xl:`, and the overriding `dark:`
        // keeps the builtin's order (after the breakpoints).
        assert_eq!(
            sort("bg-brand center-flex p-4 3xl:p-8 2xl:p-6 dark:p-2", &reg),
            "center-flex bg-brand p-4 2xl:p-6 3xl:p-8 dark:p-2"
        );
        let empty = TailwindRegistry::new();
        assert_eq!(
            sort("bg-brand center-flex p-4 3xl:p-8 2xl:p-6 dark:p-2", &empty),
            "bg-brand center-flex 3xl:p-8 p-4 2xl:p-6 dark:p-2"
        );
    }
}
