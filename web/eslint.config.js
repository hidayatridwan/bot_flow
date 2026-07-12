import prettier from 'eslint-config-prettier';
import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import { defineConfig } from 'eslint/config';
import globals from 'globals';
import ts from 'typescript-eslint';

export default defineConfig(
	// This used to be `includeIgnoreFile(path.resolve(import.meta.dirname, '.gitignore'))`, but this
	// repo keeps a single .gitignore at the workspace root — so eslint threw on a file that was never
	// there, and linted nothing. includeIgnoreFile only works with an ignore file beside the config,
	// so the patterns are listed here rather than duplicating the root .gitignore into web/.
	{ ignores: ['.svelte-kit/', 'build/', 'node_modules/'] },
	js.configs.recommended,
	ts.configs.recommended,
	svelte.configs.recommended,
	prettier,
	svelte.configs.prettier,
	{
		languageOptions: { globals: { ...globals.browser, ...globals.node } },
		rules: {
			// typescript-eslint strongly recommend that you do not use the no-undef lint rule on TypeScript projects.
			// see: https://typescript-eslint.io/troubleshooting/faqs/eslint/#i-get-errors-from-the-no-undef-rule-about-global-variables-not-being-defined-even-though-there-are-no-typescript-errors
			'no-undef': 'off'
		}
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
		languageOptions: {
			parserOptions: {
				projectService: true,
				extraFileExtensions: ['.svelte'],
				parser: ts.parser
			}
		}
	},
	{
		// shadcn-svelte generates these and `shadcn-svelte add` overwrites them. They are not ours to
		// hand-edit, so they are not ours to lint — linting them only tempts someone into editing them.
		files: ['src/lib/components/ui/**'],
		rules: { 'svelte/no-navigation-without-resolve': 'off' }
	},
	{
		// The sidebar nav is still mock data: every item is a placeholder `href="#"` with nothing
		// behind it. resolve() cannot type a route that does not exist. Drop this override as each
		// section gets a real route.
		files: ['src/lib/components/nav-main.svelte', 'src/lib/components/nav-projects.svelte'],
		rules: { 'svelte/no-navigation-without-resolve': 'off' }
	},
	{
		// Override or add rule settings here, such as:
		// 'svelte/button-has-type': 'error'
		rules: {}
	}
);
