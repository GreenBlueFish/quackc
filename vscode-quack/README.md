# Quack language support for VS Code

Syntax highlighting, bracket/comment behavior, and snippets for the **Quack**
space-time (s-t) algebra language used by the `quackc` compiler. Activates on
`.sta` files.

## Features

- **Syntax highlighting** for keywords (`module`, `wire`, `inst`, `func`, `test`,
  `wave`, `expect`, `for`, `in`, `if`, `else`, `return`, `param`, `import`, `include`, `cases`,`n  `default`, `assert`, `input`, `output`, reserved `stage`/`reset`),
  the `time` type, the `inf` literal, integer literals, the s-t relational
  operators `<` `>` `<=` `>=` `==` `!=` `^`, arithmetic, `->`, `..`, comments, module/instance
  names, function calls, and named port connections.
- **Editing behavior**: bracket matching and auto-closing for `()` `[]` `{}`,
  line (`//`) and block (`/* */`) comment toggling, and indentation.
- **Snippets**: `module`, `modulep`, `func`, `wire`, `wirev`, `inst`, `for`, `if`,
  `import`, `test`, `wave`.

## Install locally

From this folder:

```sh
npm install -g @vscode/vsce   # one time
vsce package                  # produces quack-lang-0.1.0.vsix
```

Then in VS Code / Kiro: Extensions panel -> "..." menu -> **Install from VSIX...**
and pick the generated `.vsix`.

For a quick dev loop without packaging, copy (or symlink) this folder into your
extensions directory:

- Windows: `%USERPROFILE%\.vscode\extensions\quack-lang`

Reload the window and open any `.sta` file.

## Scope

This is a lightweight (no-server) extension. Live diagnostics, go-to-definition,
and hover would require a language server wrapping `quackc` — a possible future
addition.
