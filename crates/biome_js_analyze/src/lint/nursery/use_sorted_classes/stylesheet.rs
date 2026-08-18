//! Resolves the rule's `stylesheet` option to a [TailwindRegistry]
//! through the module graph.
//!
//! The stylesheet and the partials it `@import`s are files the workspace
//! has already indexed and parsed (the rule runs in the `Project`
//! domain), so the registry is a Salsa query over their [ModuleInfo]:
//! computed once per stylesheet, shared by every class attribute in the
//! project, and recomputed only when one of the CSS files changes.

use biome_css_parser::{CssParserOptions, parse_css};
use biome_css_syntax::{
    AnyCssRoot, CssSyntaxNode, TwCustomVariantAtRule, TwThemeAtRule, TwUtilityAtRule,
};
use biome_languages::CssFileSource;
use biome_module_graph::{ModuleDb, ModuleInfo};
use biome_rowan::AstNode;
use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashSet;

use biome_tailwind_logic::sorted_classes::{TailwindRegistry, extract_tailwind_registry};

/// Everything `@utility`, `@custom-variant`, and `@theme` register in the
/// stylesheet at `module` and the stylesheets it imports, in the order
/// Tailwind sees them: each `@import` expands in place, so an imported
/// partial's directives land before the importing file's own.
///
/// `@import "tailwindcss"` (and `tailwindcss/…`) is skipped: it is the
/// preset the sorter already carries.
#[salsa::tracked(returns(ref))]
pub(super) fn tailwind_registry_for_stylesheet(
    db: &dyn ModuleDb,
    module: ModuleInfo,
) -> TailwindRegistry {
    let mut registry = TailwindRegistry::new();
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    // Depth-first, imports before the importing file: `Enter` queues a
    // file's imports (first import on top) with the file's own `Extract`
    // beneath them, so a file is extracted after everything it imports.
    let mut stack = vec![Frame::Enter(module.path(db).clone())];

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter(path) => {
                if !visited.insert(path.clone()) {
                    continue;
                }
                let Some(css) = db.css_module_info_for_path(&path) else {
                    continue;
                };
                stack.push(Frame::Extract(path));
                for import in css.imports.iter().rev() {
                    if is_tailwind_package(import.specifier.text()) {
                        continue;
                    }
                    if let Some(target) = import.resolved_path.as_path() {
                        stack.push(Frame::Enter(target.to_path_buf()));
                    }
                }
            }
            Frame::Extract(path) => {
                if let Some(root) = parsed_stylesheet(db, &path) {
                    extract_tailwind_registry(&root, &mut registry);
                }
            }
        }
    }

    registry
}

enum Frame {
    Enter(Utf8PathBuf),
    Extract(Utf8PathBuf),
}

fn is_tailwind_package(specifier: &str) -> bool {
    specifier == "tailwindcss" || specifier.starts_with("tailwindcss/")
}

/// The indexed parse of a CSS file, re-parsed with Tailwind directives
/// enabled when the workspace parsed it without them: the rule's option
/// names the file as a Tailwind stylesheet, so its `@utility` blocks are
/// meant to be read whatever `css.parser.tailwindDirectives` says.
fn parsed_stylesheet(db: &dyn ModuleDb, path: &Utf8Path) -> Option<AnyCssRoot> {
    let source = db.parsed_source_for_path(path)?;
    let root: AnyCssRoot = source.parsed(db).tree();
    if has_tailwind_directives(root.syntax()) {
        return Some(root);
    }
    let text = root.syntax().text_with_trivia().to_string();
    if !["@utility", "@theme", "@custom-variant"]
        .iter()
        .any(|directive| text.contains(directive))
    {
        return Some(root);
    }
    let options = CssParserOptions::default().allow_tailwind_directives();
    Some(parse_css(&text, CssFileSource::css(), options).tree())
}

fn has_tailwind_directives(root: &CssSyntaxNode) -> bool {
    root.descendants().any(|node| {
        TwUtilityAtRule::can_cast(node.kind())
            || TwThemeAtRule::can_cast(node.kind())
            || TwCustomVariantAtRule::can_cast(node.kind())
    })
}
