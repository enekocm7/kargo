--- The `kargo` language server for Neovim 0.11+. Neovim finds this file in the `lsp/` directory of
--- the plugin; enable the server with `vim.lsp.enable('kargo')`.
return {
  cmd = { 'kargo', 'lsp' },
  filetypes = { 'kotlin', 'toml' },
  root_markers = { 'kargo.lock', 'kargo.toml' },
}
