// Ground-truth generator for `crates/biome_js_analyze/tests/sort_v4/stylesheet/`.
//
// Loads Tailwind's design system with `@import "tailwindcss"` followed by
// a project stylesheet, then sorts each case the way
// `prettier-plugin-tailwindcss` does: `getClassOrder`, unknown classes
// first in source order, the rest by their bigint order. Prints the same
// `input:` / `sorted:` rendering as the Rust snapshot so the two can be
// diffed directly.
//
// Usage: tsx src/v4/oracle-with-css.ts <fixture.css> <fixture.jsonc>

import fs from "node:fs/promises";
import path from "node:path";
import { __unstable__loadDesignSystem } from "tailwindcss";
import { makeLoadStylesheet } from "./css-helpers.js";

function bigSign(value: bigint): number {
	return value > 0n ? 1 : value < 0n ? -1 : 0;
}

function parseJsonc(raw: string): string[] {
	// Strip `//` comments outside strings, then trailing commas.
	let out = "";
	let inString = false;
	for (let i = 0; i < raw.length; i++) {
		const c = raw[i];
		if (inString) {
			out += c;
			if (c === "\\") {
				out += raw[++i];
			} else if (c === '"') {
				inString = false;
			}
			continue;
		}
		if (c === '"') {
			inString = true;
			out += c;
		} else if (c === "/" && raw[i + 1] === "/") {
			while (i < raw.length && raw[i] !== "\n") i++;
			out += "\n";
		} else {
			out += c;
		}
	}
	return JSON.parse(out.replace(/,\s*]/g, "]"));
}

async function main() {
	const [cssPath, casesPath] = process.argv.slice(2);
	if (!cssPath || !casesPath) {
		console.error("usage: oracle-with-css.ts <fixture.css> <fixture.jsonc>");
		process.exit(1);
	}
	const stylesheet = await fs.readFile(cssPath, "utf8");
	const cases = parseJsonc(await fs.readFile(casesPath, "utf8"));
	const css = `@import "tailwindcss";\n${stylesheet}`;
	const ds = await __unstable__loadDesignSystem(css, {
		base: path.dirname(path.resolve(cssPath)),
		loadStylesheet: makeLoadStylesheet(),
	});

	const rendered = cases.map((input) => {
		const classes = input.split(/\s+/).filter(Boolean);
		const ordered = ds.getClassOrder(classes);
		ordered.sort(([, a], [, z]) => {
			if (a === z) return 0;
			if (a === null) return -1;
			if (z === null) return 1;
			return bigSign(a - z);
		});
		const sorted = ordered.map(([name]) => name).join(" ");
		return `input:  ${input}\nsorted: ${sorted}`;
	});
	console.log(rendered.join("\n---\n"));
}

void main();
