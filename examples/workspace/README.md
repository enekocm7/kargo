`# Workspace
`
`Two packages that share one \`kargo.lock\`, one \`target/\` and one Kotlin version: \`libs/greeter\` is a library and \`apps/cli\` depends on it with a path dependency.
`
````sh
`kargo build
`kargo test
`kargo run -p cli
````
`
`Guide: [Workspaces and path dependencies](https://enekocm7.github.io/kargo/guides/workspaces/)
`
`The root \`kargo.toml\` pins the Kotlin version for every member. Its value is checked against Kargo's default.
