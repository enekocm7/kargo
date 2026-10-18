--- Registers `:Kargo`. The module behind it loads on first use, so the plugin costs nothing at startup.
if vim.g.loaded_kargo then
  return
end
vim.g.loaded_kargo = true

vim.api.nvim_create_user_command('Kargo', function(opts)
  require('kargo').command(opts.fargs)
end, {
  nargs = '*',
  desc = 'Run a Kargo command: build, check and test report to the quickfix list; others run in a terminal',
  complete = function(lead, line)
    return require('kargo').complete(lead, line)
  end,
})
