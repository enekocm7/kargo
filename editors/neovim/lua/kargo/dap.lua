--- nvim-dap configurations for Kargo. The `kargo` adapter runs `kargo dap` in the project, which
--- launches `kargo run` or `kargo test` with `--debug`, or attaches to a JVM already waiting for a
--- debugger on port 5005.
local M = {}

--- The configurations `setup()` adds to the `kotlin` filetype.
local configurations = {
  { type = 'kargo', request = 'launch', name = 'kargo run', args = { 'run' } },
  { type = 'kargo', request = 'launch', name = 'kargo test', args = { 'test' } },
  { type = 'kargo', request = 'attach', name = 'Attach to port 5005', port = 5005 },
}

--- Registers the `kargo` adapter and its configurations with nvim-dap. Running it again replaces them.
function M.setup()
  local ok, dap = pcall(require, 'dap')
  if not ok then
    return vim.notify('kargo: the debugger needs nvim-dap (mfussenegger/nvim-dap)', vim.log.levels.ERROR)
  end
  local cmd = require('kargo').config.cmd
  dap.adapters.kargo = function(callback, config)
    -- `kargo dap` debugs the project of its working directory, so it starts in the project.
    callback({
      type = 'executable',
      command = cmd,
      args = { 'dap' },
      options = { cwd = config.cwd or vim.fs.root(0, { 'kargo.lock', 'kargo.toml' }) or vim.fn.getcwd() },
    })
  end
  local kept = vim.tbl_filter(function(config)
    return config.type ~= 'kargo'
  end, dap.configurations.kotlin or {})
  dap.configurations.kotlin = vim.list_extend(kept, vim.deepcopy(configurations))
end

return M
