--- Kargo for Neovim: the `:Kargo` command. `build`, `check` and `test` report into the quickfix
--- list and as diagnostics; every other command runs in a terminal split. The runnables of the
--- project come from `kargo lsp`, which the `kargo` language server (see `lsp/kargo.lua`) runs.
local messages = require('kargo.messages')

local M = {}

--- The options of `setup()`: `cmd` is the `kargo` executable, `height` the rows of a terminal split.
M.config = { cmd = 'kargo', height = 12 }

--- The subcommands whose output is parsed; their messages go to the quickfix list.
local reporting = { build = true, check = true, test = true }

--- The subcommands `:Kargo <TAB>` completes.
local subcommands = { 'build', 'check', 'test', 'run', 'clean', 'add', 'remove', 'update', 'fetch', 'task', 'tree', 'runnables' }

local namespace = vim.api.nvim_create_namespace('kargo.build')

---@param opts? { cmd?: string, height?: integer }
function M.setup(opts)
  M.config = vim.tbl_deep_extend('force', M.config, opts or {})
  if vim.lsp.config then
    -- The language server runs the same executable as the commands.
    vim.lsp.config('kargo', { cmd = { M.config.cmd, 'lsp' } })
  end
end

---@param text string
---@param level? integer
local function notify(text, level)
  vim.notify('kargo: ' .. text, level or vim.log.levels.INFO)
end

--- The nearest `kargo.toml` above the current buffer's file, or above the working directory.
---@return string?
local function manifest()
  local file = vim.api.nvim_buf_get_name(0)
  local from = file ~= '' and vim.fs.dirname(file) or vim.fn.getcwd()
  return vim.fs.find('kargo.toml', { upward = true, path = from })[1]
end

--- The package directory commands run in: the one of `manifest()`, else the working directory.
---@return string
local function project_root()
  local found = manifest()
  return found and vim.fs.dirname(found) or vim.fn.getcwd()
end

--- The `kargo` language server attached to the current buffer, if it is running.
---@return vim.lsp.Client?
local function client()
  return vim.lsp.get_clients({ name = 'kargo', bufnr = 0 })[1]
end

--- Runs `argv` in a terminal split. Buffers are reloaded afterwards, since commands such as
--- `kargo add` change `kargo.toml`.
---@param argv string[]
---@param cwd string
local function in_terminal(argv, cwd)
  vim.cmd(('botright %dnew'):format(M.config.height))
  vim.fn.jobstart(argv, {
    term = true,
    cwd = cwd,
    on_exit = function(_, code)
      vim.schedule(function()
        vim.cmd.checktime()
        if code ~= 0 then
          notify(('`%s` exited with code %d'):format(table.concat(argv, ' ', 2), code), vim.log.levels.WARN)
        end
      end)
    end,
  })
end

--- Runs `argv` without a terminal and hands its output to `on_done`, on the main loop.
---@param argv string[]
---@param cwd string
---@param on_done fun(stdout: string, stderr: string, code: integer)
local function capture(argv, cwd, on_done)
  vim.system(argv, { cwd = cwd, text = true }, function(result)
    vim.schedule(function()
      on_done(result.stdout or '', result.stderr or '', result.code)
    end)
  end)
end

--- The last line Kargo printed to stderr, which explains a failure that left no messages.
---@param stderr string
---@return string?
local function last_line(stderr)
  local last
  for line in vim.gsplit(stderr, '\n', { plain = true, trimempty = true }) do
    last = line
  end
  return last
end

--- Runs `kargo <subcommand> --message-format json <args>` and reports its messages: compiler
--- diagnostics and failed tests go to the quickfix list, and the diagnostics are shown in the buffers.
---@param subcommand string
---@param args string[]
local function run_reporting(subcommand, args)
  local argv = vim.list_extend({ M.config.cmd, subcommand, '--message-format', 'json' }, args)
  capture(argv, project_root(), function(stdout, stderr, code)
    local msgs = messages.decode(stdout)
    local items = messages.quickfix(msgs)
    for _, test in ipairs(messages.test_results(msgs)) do
      if test.outcome == 'failed' then
        table.insert(items, { text = ('%s.%s: %s'):format(test.suite or '', test.name, test.message or 'failed'), type = 'E' })
      end
    end

    vim.fn.setqflist({}, ' ', { title = 'kargo ' .. subcommand, items = items })
    vim.diagnostic.reset(namespace)
    for file, diagnostics in pairs(messages.diagnostics(msgs)) do
      local bufnr = vim.fn.bufadd(file)
      vim.fn.bufload(bufnr)
      vim.diagnostic.set(namespace, bufnr, diagnostics)
    end
    if #items > 0 then
      vim.cmd('copen')
    end

    if subcommand == 'test' and #msgs > 0 then
      local summary = messages.summary(msgs)
      local text = ('%d passed, %d failed, %d ignored'):format(summary.passed, summary.failed, summary.ignored)
      notify(text, code == 0 and vim.log.levels.INFO or vim.log.levels.WARN)
    elseif code ~= 0 then
      local detail = #items > 0 and ('%d problems'):format(#items) or last_line(stderr) or 'no output'
      notify(('%s failed: %s'):format(subcommand, detail), vim.log.levels.WARN)
    else
      notify(subcommand .. ' finished')
    end
  end)
end

--- Adds dependencies through `kargo/addDependency`, which edits `kargo.toml` in the editor, or
--- through `kargo add` in a terminal when no Kargo language server is attached.
---@param coords string[]
local function add(coords)
  local lsp = client()
  if not lsp then
    return in_terminal(vim.list_extend({ M.config.cmd, 'add' }, coords), project_root())
  end
  local found = manifest()
  if not found then
    return notify('no kargo.toml above this file', vim.log.levels.ERROR)
  end
  local arguments = { { uri = vim.uri_from_fname(found), coords = #coords == 1 and coords[1] or coords } }
  lsp:request('workspace/executeCommand', { command = 'kargo/addDependency', arguments = arguments }, function(err, result)
    vim.schedule(function()
      if err then
        return notify(err.message, vim.log.levels.ERROR)
      end
      notify('added ' .. table.concat(result and result.added or {}, ', '))
    end)
  end, 0)
end

--- Asks for coordinates, then adds them.
local function add_interactively()
  vim.ui.input({ prompt = 'Coordinates (group:artifact[:version]): ' }, function(input)
    if input and input ~= '' then
      add(vim.split(input, '%s+', { trimempty = true }))
    end
  end)
end

--- Lists the runnables of the buffer's package (programs, tests and tasks) and runs the one picked.
function M.runnables()
  local lsp = client()
  if not lsp then
    return notify('kargo lsp is not attached to this buffer, so there are no runnables', vim.log.levels.WARN)
  end
  lsp:request('kargo/runnables', { textDocument = { uri = vim.uri_from_bufnr(0) } }, function(err, result)
    vim.schedule(function()
      if err then
        return notify(err.message, vim.log.levels.ERROR)
      end
      vim.ui.select(result or {}, {
        prompt = 'Kargo',
        format_item = function(item)
          return item.label
        end,
      }, function(item)
        if item then
          in_terminal(vim.list_extend({ item.program }, item.args), item.cwd)
        end
      end)
    end)
  end, 0)
end

--- Runs `:Kargo <subcommand> [args]`.
---@param fargs string[]
function M.command(fargs)
  local subcommand = table.remove(fargs, 1)
  if subcommand == nil then
    return notify('usage: :Kargo <' .. table.concat(subcommands, '|') .. '> [args]', vim.log.levels.WARN)
  end
  if reporting[subcommand] then
    return run_reporting(subcommand, fargs)
  end
  if subcommand == 'runnables' then
    return M.runnables()
  end
  if subcommand == 'add' then
    if #fargs == 0 then
      return add_interactively()
    end
    return add(fargs)
  end
  in_terminal(vim.list_extend({ M.config.cmd, subcommand }, fargs), project_root())
end

--- Completes the subcommand of `:Kargo`.
---@param lead string
---@param line string
---@return string[]
function M.complete(lead, line)
  local before = line:sub(1, #line - #lead)
  if #vim.split(before, '%s+', { trimempty = true }) > 1 then
    return {}
  end
  return vim.tbl_filter(function(name)
    return vim.startswith(name, lead)
  end, subcommands)
end

return M
