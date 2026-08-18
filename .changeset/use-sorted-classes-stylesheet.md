---
"@biomejs/biome": minor
---

Added the top-level `tailwind.stylesheet` configuration: [`useSortedClasses`](https://biomejs.dev/linter/rules/use-sorted-classes/) reads the `@theme`, `@utility`, and `@custom-variant` directives of the project's Tailwind CSS stylesheet (and the stylesheets it imports) and sorts custom utilities, variants, breakpoints, and theme keys the way Tailwind does.

```json
{
  "tailwind": {
    "stylesheet": "src/app.css"
  }
}
```
