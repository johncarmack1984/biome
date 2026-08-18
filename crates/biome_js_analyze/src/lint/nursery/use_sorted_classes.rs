// `sort` owns only the engine-independent diagnostic-range and
// template-literal helpers; the class sorting itself lives in
// `biome_tailwind_logic::sorted_classes`, shared with the HTML rule, and
// `stylesheet` resolves the project's stylesheet to the `TailwindRegistry`
// it sorts against.
mod sort;
mod stylesheet;

use self::{
    sort::TemplateLiteralSpaceContext, sort::get_sort_class_name_range,
    stylesheet::tailwind_registry_for_stylesheet,
};
use crate::JsRuleAction;
use crate::services::database::ResolvedImports;
use crate::shared::any_class_string_like::AnyClassStringLike;
use biome_analyze::{
    FixKind, Rule, RuleDiagnostic, RuleDomain, context::RuleContext, declare_lint_rule,
};
use biome_console::markup;
use biome_js_factory::make::{
    js_literal_member_name, js_string_literal, js_string_literal_expression,
    js_string_literal_single_quotes, js_template_chunk, js_template_chunk_element, jsx_string,
};
use biome_rowan::{AstNode, BatchMutationExt, TokenText};
use biome_rule_options::use_sorted_classes::UseSortedClassesOptions;
use biome_tailwind_logic::sorted_classes::{EMPTY_REGISTRY, TailwindRegistry, sort_class_string};

declare_lint_rule! {
    /// Enforce the sorting of CSS utility classes.
    ///
    /// This rule sorts classes the way [Tailwind CSS v4](https://tailwindcss.com/blog/automatic-class-sorting-with-prettier#how-classes-are-sorted) and its Prettier plugin do: utilities in the order Tailwind emits them, variants after plain utilities and grouped by variant, and classes the rule doesn't recognize kept at the front in their original order.
    ///
    /// It is analogous to [`prettier-plugin-tailwindcss`](https://github.com/tailwindlabs/prettier-plugin-tailwindcss).
    ///
    ///
    /// :::caution
    /// ## Important notes
    ///
    /// Progress on this rule is tracked in the following GitHub issue: https://github.com/biomejs/biome/issues/1274
    ///
    /// Currently, utility class sorting is **not part of the formatter**, and is implemented as a linter rule instead, with an automatic fix. The fix is, at this stage, classified as unsafe. This means that **it won't be applied automatically** as part of IDE actions such as "fix on save".
    ///
    /// We appreciate any feedback on this rule, and encourage you to try it out and report any issues you find.
    ///
    /// **Please read this entire documentation page before reporting an issue.**
    ///
    /// Notably, keep in mind that the following features are not supported yet:
    ///
    /// - The `prefix` option and Tailwind CSS v3 configuration files.
    /// - Utilities and variants added by JavaScript (`@plugin`, `@config`), which Biome cannot execute.
    ///
    /// Please don't report issues about these features.
    /// :::
    ///
    /// ## Examples
    ///
    /// ### Invalid
    ///
    /// ```jsx,expect_diagnostic
    /// <div class="px-2 foo p-4 bar" />;
    /// ```
    ///
    /// ```jsx,expect_diagnostic
    /// <div class="hover:focus:m-2 foo hover:px-2 p-4" />
    /// ```
    ///
    /// ## Options
    ///
    /// ### Code-related
    ///
    /// ```json,options
    /// {
    ///     "options": {
    ///         "attributes": ["classList"],
    ///         "functions": ["clsx", "cva", "tw", "tw.*"]
    ///     }
    /// }
    /// ```
    ///
    /// #### attributes
    ///
    /// Classes in the `class` and `className` JSX attributes are always sorted. Use this option to add more attributes that should be sorted.
    ///
    /// #### functions
    ///
    /// If specified, strings in the indicated functions will be sorted. This is useful when working with libraries like [`clsx`](https://github.com/lukeed/clsx) or [`cva`](https://cva.style/).
    ///
    /// ```js,expect_diagnostic,use_options
    /// clsx("px-2 foo p-4 bar", {
    ///     "some-css-class": condition,
    /// });
    /// ```
    ///
    /// ```js,expect_diagnostic,use_options
    /// clsx("some-css-class", {
    ///     "block mx-4": condition,
    /// });
    /// ```
    ///
    /// Tagged template literals are also supported, for example:
    ///
    /// ```js,use_options
    /// tw`px-2`;
    /// tw.div`px-2`;
    /// ```
    ///
    /// ```js,expect_diagnostic,use_options
    /// tw`px-2 foo p-4 bar`;
    /// ```
    ///
    /// ```js,expect_diagnostic,use_options
    /// tw.div`px-2 foo p-4 bar`;
    /// ```
    ///
    /// ### Sort-related
    ///
    /// The sort order is configured through the top-level `tailwind` configuration rather than through rule options.
    ///
    /// #### `tailwind.stylesheet`
    ///
    /// Path to the project's Tailwind CSS entry stylesheet, relative to the root of the package that contains the linted file (the directory of its nearest `package.json`, or the working directory when there is none). This mirrors the `tailwindStylesheet` option of `prettier-plugin-tailwindcss`.
    ///
    /// When set, the rule reads the `@theme`, `@utility`, and `@custom-variant` directives of that file and of the stylesheets it `@import`s, and sorts with them:
    ///
    /// - `@theme` keys make values valid (`bg-brand` for `--color-brand`), add breakpoints (`3xl:` for `--breakpoint-3xl`) and container sizes, and `--color-*: initial` style resets drop Tailwind's defaults.
    /// - `@utility` registrations place custom classes by the properties they set, the way Tailwind orders them; functional utilities (`@utility tab-*`) accept the values their `--value()` and `--modifier()` calls describe.
    /// - `@custom-variant` registrations sort after Tailwind's own variants, in the order they are declared; redefining a builtin (`@custom-variant dark (…)`) keeps its place.
    ///
    /// The stylesheet must be part of the project Biome scans (it cannot be excluded through `files.includes`), and the rule enables the CSS parser's Tailwind syntax for it regardless of `css.parser.tailwindDirectives`. If the file cannot be found, the rule falls back to Tailwind's defaults.
    ///
    /// ```json
    /// {
    ///     "tailwind": {
    ///         "stylesheet": "src/app.css"
    ///     }
    /// }
    /// ```
    ///
    /// Without it, the rule sorts with Tailwind's default v4 theme built in.
    ///
    /// ## Differences with [Prettier](https://github.com/tailwindlabs/prettier-plugin-tailwindcss)
    ///
    /// The main key difference is that Tailwind CSS and its Prettier plugin load your stylesheet through Tailwind itself, executing any JavaScript it pulls in (`@plugin`, `@config`, and for v3 `tailwind.config.js`), which Biome doesn't do. Instead, Biome sorts with Tailwind's default theme built in and reads the CSS-declared parts of your stylesheet through the top-level `tailwind.stylesheet` configuration. The trade-offs are explained below.
    ///
    /// ### Only CSS-declared additions are read
    ///
    /// The rule knows the utilities, variants, breakpoints, and theme values that Tailwind CSS v4 ships with, and orders them the way Tailwind does. With `tailwind.stylesheet` configured it also knows what your CSS declares. Anything added by JavaScript — `@plugin`, `@config`, or a v3 config file — and the `prefix` option remain unknown to it, and classes it does not recognize are left where the Prettier plugin would put them: at the front, in their original order.
    ///
    /// ### Whitespace is collapsed
    ///
    /// The Tailwind CSS Prettier plugin preserves all original whitespace. This rule, however, collapses all whitespace (including newlines) into single spaces.
    ///
    /// This is a deliberate decision. We're unsure about this behavior, and would appreciate feedback on it. If this is a problem for you, please share a detailed explanation of your use case in [the GitHub issue](https://github.com/biomejs/biome/issues/1274).
    ///
    pub UseSortedClasses {
        version: "1.6.0",
        name: "useSortedClasses",
        language: "js",
        recommended: false,
        fix_kind: FixKind::Unsafe,
        issue_number: Some("1274"),
        domains: &[RuleDomain::Project],
    }
}

/// Sort a class string with the Tailwind v4 engine, preserving the
/// template-literal semantics the v3 path handled: a class glued to a
/// `${…}` interpolation is held out of sorting, and a boundary space next
/// to an interpolation is kept. Only the sortable middle goes through
/// [`sort_class_string`].
fn sort_class_name_v4(
    class_name: &TokenText,
    template_ctx: &Option<TemplateLiteralSpaceContext>,
    registry: &TailwindRegistry,
) -> String {
    let (ignore_prefix, ignore_postfix) = template_ctx
        .as_ref()
        .map_or((false, false), |ctx| ctx.get_ignore_flags());

    let mut classes = class_name.split_whitespace();
    let prefix = ignore_prefix.then(|| classes.next()).flatten();
    let postfix = ignore_postfix.then(|| classes.next_back()).flatten();

    let middle = classes.collect::<Vec<_>>().join(" ");
    let sorted_middle = sort_class_string(&middle, registry);

    let mut parts: Vec<&str> = Vec::with_capacity(3);
    parts.extend(prefix);
    if !sorted_middle.is_empty() {
        parts.push(sorted_middle.as_str());
    }
    parts.extend(postfix);
    let mut result = parts.join(" ");

    if let Some(ctx) = template_ctx {
        if ctx.keep_leading() {
            result.insert(0, ' ');
        }
        if ctx.keep_trailing() {
            result.push(' ');
        }
    }
    result
}

impl Rule for UseSortedClasses {
    type Query = ResolvedImports<AnyClassStringLike>;
    type State = Box<str>;
    type Signals = Option<Self::State>;
    type Options = UseSortedClassesOptions;

    fn run(ctx: &RuleContext<Self>) -> Option<Self::State> {
        let options = ctx.options();
        let node = ctx.query();

        if node.should_visit(options)?
            && let Some(value) = node.value()
        {
            let registry = stylesheet_registry(ctx).unwrap_or(&EMPTY_REGISTRY);
            let template_ctx = sort::get_template_literal_space_context(node);
            let sorted_value: String = sort_class_name_v4(&value, &template_ctx, registry);
            if sorted_value.is_empty() {
                return None;
            }
            if value.text() != sorted_value {
                return Some(sorted_value.into());
            }
        }
        None
    }

    fn diagnostic(ctx: &RuleContext<Self>, _: &Self::State) -> Option<RuleDiagnostic> {
        let node = ctx.query();

        // Calculate the range offset to account for the ignored prefix and postfix.
        let sort_range = if let Some(value) = node.value() {
            let range = node.range();
            let template_ctx = sort::get_template_literal_space_context(node);
            let real_sort_range = get_sort_class_name_range(&value, &range, &template_ctx);
            real_sort_range.unwrap_or(range)
        } else {
            node.range()
        };

        Some(RuleDiagnostic::new(
            rule_category!(),
            sort_range,
            "These CSS classes should be sorted.",
        ))
    }

    fn action(ctx: &RuleContext<Self>, state: &Self::State) -> Option<JsRuleAction> {
        let mut mutation = ctx.root().begin();
        match ctx.query() {
            AnyClassStringLike::JsStringLiteralExpression(string_literal) => {
                let is_double_quote = string_literal
                    .value_token()
                    .map_or(ctx.preferred_quote().is_double(), |token| {
                        token.text_trimmed().starts_with('"')
                    });
                let replacement = js_string_literal_expression(if is_double_quote {
                    js_string_literal(state)
                } else {
                    js_string_literal_single_quotes(state)
                });
                mutation.replace_node(string_literal.clone(), replacement);
            }
            AnyClassStringLike::JsLiteralMemberName(string_literal) => {
                let replacement = js_literal_member_name(if ctx.preferred_quote().is_double() {
                    js_string_literal(state)
                } else {
                    js_string_literal_single_quotes(state)
                });
                mutation.replace_node(string_literal.clone(), replacement);
            }
            AnyClassStringLike::JsxString(jsx_string_node) => {
                let is_double_quote = jsx_string_node
                    .value_token()
                    .map_or(ctx.preferred_jsx_quote().is_double(), |token| {
                        token.text_trimmed().starts_with('"')
                    });
                let replacement = jsx_string(if is_double_quote {
                    js_string_literal(state)
                } else {
                    js_string_literal_single_quotes(state)
                });
                mutation.replace_node(jsx_string_node.clone(), replacement);
            }
            AnyClassStringLike::JsTemplateChunkElement(chunk) => {
                let replacement = js_template_chunk_element(js_template_chunk(state));
                mutation.replace_node(chunk.clone(), replacement);
            }
        };

        Some(JsRuleAction::new(
            ctx.metadata().action_category(ctx.category(), ctx.group()),
            ctx.metadata().applicability(),
            markup! {
                "Sort the classes."
            }
            .to_owned(),
            mutation,
        ))
    }
}

/// The registry for `tailwind.stylesheet`, or `None` when it is unset or
/// names a file the workspace has not indexed. The path is relative to
/// the package root of the linted file (falling back to the working
/// directory), and the registry itself is a memoized query over the
/// module graph, so this costs a path join and a lookup per class
/// attribute.
fn stylesheet_registry<'a>(ctx: &'a RuleContext<UseSortedClasses>) -> Option<&'a TailwindRegistry> {
    let stylesheet = ctx.tailwind().stylesheet()?;

    let file_path = ctx.file_path();
    let package_dir = ctx
        .project_layout()
        .find_node_manifest_for_path(file_path)
        .map(|(dir, _)| dir)
        .or_else(|| ctx.working_directory().map(|p| p.to_path_buf()))?;
    let css_path = package_dir.join(stylesheet);

    let db = ctx.db();
    let module = db.module_for_path(&css_path)?;
    Some(tailwind_registry_for_stylesheet(db, module))
}
