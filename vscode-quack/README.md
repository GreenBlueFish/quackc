# Quack language support for VS Code

Syntax highlighting, bracket/comment behavior, and snippets for the **Quack**
space-time (s-t) algebra language used by the `quackc` compiler. Activates on
`.sta` files.

## Features

- **Syntax highlighting** for keywords (`module`, `wire`, `inst`, `func`, `test`,
  `wave`, `expect`, `for`, `in`, `if`, `else`, `return`, `param`, `import`, `include`, `cases`, `default`, `assert`, `input`, `output`, reserved `stage`/`reset`),
  the `time` type, the `inf` literal, integer literals, the s-t relational
  operators `<` `>` `<=` `>=` `==` `!=` `^`, arithmetic, `->`, `..`, comments, module/instance
  names, function calls, and named port connections.
- **Editing behavior**: bracket matching and auto-closing for `()` `[]` `{}`,
  line (`//`) and block (`/* */`) comment toggling, and indentation.
- **Snippets**: `module`, `modulep`, `func`, `wire`, `wirev`, `inst`, `for`, `if`,
  `import`, `test`, `wave`.

## Install the prebuilt extension

A packaged `.vsix` is committed to this folder, so you can install without building:

```sh
code --install-extension quack-lang-0.5.0.vsix
```

Or in VS Code / Kiro: Extensions panel -> "..." menu -> **Install from VSIX...**
and pick `quack-lang-0.5.0.vsix`. Then run **Developer: Reload Window** and open any `.sta` file.

## Build it yourself

If you change the grammar/snippets and want to repackage:

```sh
npm install -g @vscode/vsce   # one time
vsce package --allow-missing-repository   # produces quack-lang-<version>.vsix
```

For a quick dev loop without packaging, copy (or symlink) this folder into your
extensions directory, then reload the window:

- Windows: `%USERPROFILE%\.vscode\extensions\quack-lang`
- macOS / Linux: `~/.vscode/extensions/quack-lang`

## Scope

This is a lightweight (no-server) extension. Live diagnostics, go-to-definition,
and hover would require a language server wrapping `quackc` — a possible future
addition.
