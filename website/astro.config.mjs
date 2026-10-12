// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The Pages workflow sets SITE and BASE_PATH from actions/configure-pages: on
// enekocm7.github.io the site lives under /kargo, while a custom domain (or a
// local build) serves it from the root.
export default defineConfig({
	site: process.env.SITE || undefined,
	base: process.env.BASE_PATH || undefined,
	integrations: [
		starlight({
			title: 'Kargo',
			description: 'Cargo for Kotlin: one kargo.toml, one CLI, for the JVM, WebAssembly, Kotlin/Native and Android.',
			favicon: '/favicon.svg',
			customCss: ['./src/styles/custom.css'],
			components: {
				// Replaces the social icon row with a GitHub repository card (name, version, stars, forks).
				SocialIcons: './src/components/GitHubRepo.astro',
			},
			editLink: { baseUrl: 'https://github.com/enekocm7/kargo/edit/master/website/' },
			lastUpdated: true,
			sidebar: [
				{
					label: 'Getting started',
					items: [
						{ label: 'Introduction', slug: 'getting-started/introduction' },
						{ label: 'Installation', slug: 'getting-started/installation' },
						{ label: 'Quick start', slug: 'getting-started/quick-start' },
						{ label: 'Project layout', slug: 'getting-started/project-layout' },
					],
				},
				{
					label: 'Guides',
					items: [
						{ label: 'Your first Kotlin app', slug: 'guides/first-app' },
						{ label: 'Managing dependencies', slug: 'guides/dependencies' },
						{ label: 'Workspaces', slug: 'guides/workspaces' },
						{ label: 'Testing', slug: 'guides/testing' },
						{ label: 'API documentation', slug: 'guides/documentation' },
						{ label: 'Formatting and linting', slug: 'guides/formatting-and-linting' },
						{ label: 'Multiplatform projects', slug: 'guides/multiplatform' },
						{ label: 'WebAssembly', slug: 'guides/wasm' },
						{ label: 'Kotlin/Native', slug: 'guides/native' },
						{ label: 'Android', slug: 'guides/android' },
						{ label: 'Built-in frameworks', slug: 'guides/frameworks' },
						{ label: 'A REST API with Ktor', slug: 'guides/ktor' },
						{ label: 'Spring Boot', slug: 'guides/spring-boot' },
						{ label: 'Compose Desktop', slug: 'guides/compose' },
						{ label: 'Tasks and plugins', slug: 'guides/tasks-and-plugins' },
						{ label: 'Managing JDKs', slug: 'guides/toolchains' },
						{ label: 'Continuous integration', slug: 'guides/ci' },
						{ label: 'Build performance', slug: 'guides/build-performance' },
					],
				},
				{
					label: 'Migration',
					items: [
						{ label: 'From Gradle', slug: 'migration/from-gradle' },
						{ label: 'From Maven', slug: 'migration/from-maven' },
						{ label: 'For Rust developers', slug: 'migration/from-cargo' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Commands', slug: 'reference/commands' },
						{ label: 'kargo.toml', slug: 'reference/manifest' },
						{ label: 'kargo.lock', slug: 'reference/lockfile' },
						{ label: 'Environment variables', slug: 'reference/environment' },
						{ label: 'FAQ & troubleshooting', slug: 'reference/faq' },
						{ label: 'Contributing', slug: 'reference/contributing' },
					],
				},
			],
		}),
	],
});
