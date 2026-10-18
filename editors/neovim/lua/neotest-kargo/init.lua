--- The neotest adapter for Kargo projects. It finds the tests of the Kotlin files under `tests/` and
--- `tests-<target>/` (and of `*Test.kt` files), runs them with `kargo test --message-format json`, and
--- reports each test's result on its position in the tree.
local lib = require('neotest.lib')
local Tree = require('neotest.types').Tree
local messages = require('kargo.messages')
local scan = require('neotest-kargo.scan')

local adapter = { name = 'neotest-kargo' }

adapter.root = lib.files.match_root_pattern('kargo.toml')

---@param path string
---@return boolean
function adapter.is_test_file(path)
  if not path:match('%.kt$') then
    return false
  end
  return path:match('[/\\]tests[^/\\]*[/\\]') ~= nil or path:match('Tests?%.kt$') ~= nil
end

---@param name string
---@return boolean
function adapter.filter_dir(name)
  return name ~= 'target' and not vim.startswith(name, '.')
end

--- The id neotest keys a position by: its file, then its suite, then its name.
---@param pos table
---@return string
local function position_id(pos)
  if pos.type == 'file' then
    return pos.path
  elseif pos.type == 'namespace' then
    return pos.path .. '::' .. pos.suite
  end
  return table.concat({ pos.path, pos.suite or '', pos.name }, '::')
end

---@param lines string[]
---@param row integer 1-based
---@return integer[]
local function line_range(lines, row)
  return { row - 1, 0, row - 1, #(lines[row] or '') }
end

--- A position with the id neotest reports results under.
---@param pos table
---@return table
local function with_id(pos)
  pos.id = position_id(pos)
  return pos
end

function adapter.discover_positions(path)
  local lines = vim.fn.readfile(path)
  local found = scan.scan(lines)
  local file = with_id({ type = 'file', path = path, name = vim.fs.basename(path), package = found.package, range = { 0, 0, #lines, 0 } })
  local root = { file }

  local namespaces = {}
  for _, suite in ipairs(found.suites) do
    local node = { with_id({ type = 'namespace', path = path, name = suite.name, suite = suite.fqn, range = line_range(lines, suite.row) }) }
    namespaces[suite] = node
    table.insert(root, node)
  end
  for _, test in ipairs(found.tests) do
    local parent = test.suite and namespaces[test.suite] or root
    table.insert(parent, {
      with_id({
        type = 'test',
        path = path,
        name = test.name,
        suite = test.suite and test.suite.fqn,
        range = line_range(lines, test.row),
      }),
    })
  end
  return Tree.from_list(root, function(pos)
    return pos.id
  end)
end

--- The `kargo test` filter that selects a position: `Suite.name` for a test, the suite for a
--- class, the package for a file (nil runs every test).
---@param position table
---@return string?
local function filter_of(position)
  if position.type == 'test' then
    return position.suite and (position.suite .. '.' .. position.name) or position.name
  elseif position.type == 'namespace' then
    return position.suite
  elseif position.package ~= '' then
    return position.package
  end
end

---@param args table neotest's run arguments
function adapter.build_spec(args)
  local position = args.tree:data()
  local command = { require('kargo').config.cmd, 'test', '--message-format', 'json' }
  local filter = filter_of(position)
  if filter then
    table.insert(command, filter)
  end
  vim.list_extend(command, args.extra_args or {})
  local dir = vim.fs.dirname(position.path)
  return {
    command = command,
    cwd = adapter.root(dir) or dir,
    context = { path = position.path },
  }
end

local outcomes = { passed = 'passed', failed = 'failed', ignored = 'skipped' }

--- The 0-based line of the frame in the test's own file that the stack trace names, else the
--- line the test starts on.
---@param details? string
---@param pos table
---@return integer
local function failure_line(details, pos)
  local name = vim.fs.basename(pos.path)
  for file, line in (details or ''):gmatch('%(([%w_$%.]+%.kt):(%d+)%)') do
    if file == name then
      return tonumber(line) - 1
    end
  end
  return pos.range[1]
end

--- The status of a suite or file from its tests: failed if one failed, else passed if one passed.
---@param statuses string[]
---@return string
local function rollup(statuses)
  local status = 'skipped'
  for _, value in ipairs(statuses) do
    if value == 'failed' then
      return 'failed'
    elseif value == 'passed' then
      status = 'passed'
    end
  end
  return status
end

--- Reads the JSON `kargo test` printed and reports each test, then each suite and file, as the
--- rollup of its tests. Tests that did not run keep no result.
function adapter.results(spec, result, tree)
  local ok, lines = pcall(vim.fn.readfile, result.output)
  local msgs = messages.decode(ok and table.concat(lines, '\n') or '')

  local tests = {}
  local children = {}
  for _, node in tree:iter_nodes() do
    local pos = node:data()
    if pos.type == 'test' then
      tests[(pos.suite or '') .. '\0' .. pos.name] = pos
    end
  end

  local results = {}
  for _, test in ipairs(messages.test_results(msgs)) do
    local pos = tests[(test.suite or '') .. '\0' .. test.name]
    if pos then
      local status = outcomes[test.outcome] or 'skipped'
      local entry = { status = status }
      if status == 'failed' then
        entry.short = test.message
        entry.errors = { { message = test.message or 'failed', line = failure_line(test.details, pos) } }
      end
      results[position_id(pos)] = entry
      local ancestors = { spec.context.path }
      if pos.suite then
        table.insert(ancestors, position_id({ type = 'namespace', path = pos.path, suite = pos.suite }))
      end
      for _, id in ipairs(ancestors) do
        children[id] = children[id] or {}
        table.insert(children[id], status)
      end
    end
  end
  for id, statuses in pairs(children) do
    results[id] = results[id] or { status = rollup(statuses) }
  end
  return results
end

return adapter
