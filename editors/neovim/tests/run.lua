--- Tests of the Neovim plugin. Run from editors/neovim with:
---
---   nvim --headless -u NONE -l tests/run.lua
---
--- They need no Kargo binary and no neotest or nvim-dap: the neotest modules the adapter uses are
--- replaced by small stand-ins, and the Kotlin sources and `kargo test` output are fixtures.
local root = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.opt.runtimepath:prepend(root)

local failures = 0

local function test(name, fn)
  local ok, err = pcall(fn)
  if ok then
    print('ok    ' .. name)
  else
    failures = failures + 1
    print('FAIL  ' .. name .. '\n      ' .. tostring(err))
  end
end

local function eq(expected, actual)
  if not vim.deep_equal(expected, actual) then
    error(('expected %s\n      got      %s'):format(vim.inspect(expected), vim.inspect(actual)), 2)
  end
end

local function jsonl(...)
  local lines = {}
  for _, message in ipairs({ ... }) do
    table.insert(lines, vim.json.encode(message))
  end
  return table.concat(lines, '\n')
end

local messages = require('kargo.messages')

test('decode skips output that is not a Kargo message', function()
  local text = 'Compiling my-app\n' .. jsonl({ reason = 'build-finished', success = false }) .. '\n{"no":"reason"}\n'
  eq({ { reason = 'build-finished', success = false } }, messages.decode(text))
end)

test('quickfix maps compiler messages and leaves out the rest', function()
  local decoded = messages.decode(jsonl(
    { reason = 'compiler-message', severity = 'error', message = 'type mismatch', file = '/p/src/Main.kt', line = 3, column = 9, target = 'jvm' },
    { reason = 'compiler-message', severity = 'warning', message = 'unused', file = '/p/src/Main.kt', line = 1, column = 1, target = 'jvm' },
    { reason = 'compiler-message', severity = 'error', message = 'linker failed', file = vim.NIL, line = vim.NIL, target = 'native' },
    { reason = 'build-finished', success = false }
  ))
  eq({
    { filename = '/p/src/Main.kt', lnum = 3, col = 9, type = 'E', text = 'type mismatch' },
    { filename = '/p/src/Main.kt', lnum = 1, col = 1, type = 'W', text = 'unused' },
    { filename = '', lnum = 0, col = 0, type = 'E', text = 'linker failed' },
  }, messages.quickfix(decoded))
end)

test('diagnostics group by file with zero-based positions', function()
  local decoded = messages.decode(jsonl(
    { reason = 'compiler-message', severity = 'error', message = 'a', file = '/p/A.kt', line = 2, column = 5 },
    { reason = 'compiler-message', severity = 'warning', message = 'b', file = '/p/A.kt', line = 4, column = 1 },
    { reason = 'compiler-message', severity = 'error', message = 'no location' }
  ))
  local diagnostics = messages.diagnostics(decoded)
  eq({ '/p/A.kt' }, vim.tbl_keys(diagnostics))
  eq(1, diagnostics['/p/A.kt'][1].lnum)
  eq(4, diagnostics['/p/A.kt'][1].col)
  eq(vim.diagnostic.severity.WARN, diagnostics['/p/A.kt'][2].severity)
end)

test('test results and the summary add up over targets', function()
  local decoded = messages.decode(jsonl(
    { reason = 'test-started', suite = 'com.example.MathTest', name = 'adds' },
    { reason = 'test-finished', suite = 'com.example.MathTest', name = 'adds', ['display-name'] = 'adds()', outcome = 'passed', ['duration-ms'] = 3 },
    { reason = 'test-finished', suite = 'com.example.MathTest', name = 'divides', outcome = 'failed', message = 'expected 2', details = 'at x(MathTest.kt:9)' },
    { reason = 'test-summary', target = 'jvm', passed = 1, failed = 1, ignored = 0, success = false },
    { reason = 'test-summary', target = 'native', passed = 2, failed = 0, ignored = 1, success = true }
  ))
  eq({ 'passed', 'failed' }, { messages.test_results(decoded)[1].outcome, messages.test_results(decoded)[2].outcome })
  eq('expected 2', messages.test_results(decoded)[2].message)
  eq({ passed = 3, failed = 1, ignored = 1, success = false }, messages.summary(decoded))
end)

test('build_success is nil without a build-finished message', function()
  eq(nil, messages.build_success({}))
  eq(true, messages.build_success({ { reason = 'build-finished', success = true } }))
end)

local scan = require('neotest-kargo.scan')

test('scan finds the package, suites and tests of a Kotlin file', function()
  local found = scan.scan({
    'package com.example.app',
    '',
    'import kotlin.test.Test',
    '',
    'class MathTest {',
    '    @Test',
    '    fun adds() {}',
    '',
    '    @Test fun `divides by zero`() {}',
    '',
    '    fun helper() {}',
    '',
    '    @ParameterizedTest',
    '    fun parameterized(x: Int) {}',
    '}',
    '',
    'data class Point(val x: Int)',
    'class SubclassTest {',
    '    @Test',
    '    fun ok() {}',
    '}',
  })
  eq('com.example.app', found.package)
  eq({ 'com.example.app.MathTest', 'com.example.app.Point', 'com.example.app.SubclassTest' }, vim.tbl_map(function(s)
    return s.fqn
  end, found.suites))
  eq({ 'adds', 'divides by zero', 'parameterized', 'ok' }, vim.tbl_map(function(t)
    return t.name
  end, found.tests))
  eq(9, found.tests[2].row)
  eq('com.example.app.MathTest', found.tests[2].suite.fqn)
end)

test('scan attaches a top-level test to no suite', function()
  local found = scan.scan({ 'package p', '@Test', 'fun loose() {}' })
  eq({ 'loose' }, { found.tests[1].name })
  eq(nil, found.tests[1].suite)
end)

-- The adapter needs neotest's modules. These stand-ins keep the same shape: a tree is built from
-- nested lists of `{ data, children... }`, and iter_nodes yields nodes with data().
local function stub_tree(list, key)
  local nodes = {}
  local function build(item)
    local pos = item[1]
    local node = { data = function() return pos end, id = key(pos) }
    table.insert(nodes, node)
    for i = 2, #item do
      build(item[i])
    end
    return node
  end
  local top = build(list)
  return {
    data = function() return top.data() end,
    iter_nodes = function()
      return ipairs(nodes)
    end,
  }
end

package.preload['neotest.lib'] = function()
  return { files = { match_root_pattern = function(name) return function(dir) return vim.fs.dirname(vim.fs.find(name, { upward = true, path = dir })[1]) end end } }
end
package.preload['neotest.types'] = function()
  return { Tree = { from_list = stub_tree } }
end
local adapter = require('neotest-kargo')

local fixture_dir = vim.fn.tempname()
local project = fixture_dir .. '/app'
vim.fn.mkdir(project .. '/tests', 'p')
vim.fn.writefile({ '[package]', 'name = "app"' }, project .. '/kargo.toml')
local test_file = project .. '/tests/MathTest.kt'
vim.fn.writefile({
  'package com.example.app',
  '',
  'class MathTest {',
  '    @Test',
  '    fun adds() {}',
  '',
  '    @Test',
  '    fun divides() {}',
  '}',
}, test_file)

test('is_test_file recognises Kotlin files under tests directories and Test suffixes', function()
  eq(true, adapter.is_test_file('/p/tests/MathTest.kt'))
  eq(true, adapter.is_test_file('/p/tests-jvm/Helpers.kt'))
  eq(true, adapter.is_test_file('/p/src/MathTest.kt'))
  eq(false, adapter.is_test_file('/p/src/Main.kt'))
  eq(false, adapter.is_test_file('/p/tests/notes.md'))
end)

test('discover_positions builds file, suite and test positions', function()
  local tree = adapter.discover_positions(test_file)
  local positions = {}
  for _, node in tree:iter_nodes() do
    table.insert(positions, { node:data().type, node:data().name, node:data().id })
  end
  eq({
    { 'file', 'MathTest.kt', test_file },
    { 'namespace', 'MathTest', test_file .. '::com.example.app.MathTest' },
    { 'test', 'adds', test_file .. '::com.example.app.MathTest::adds' },
    { 'test', 'divides', test_file .. '::com.example.app.MathTest::divides' },
  }, positions)
  eq('com.example.app', tree:data().package)
end)

test('build_spec runs the test, the class or the package of the position', function()
  local tree = adapter.discover_positions(test_file)
  local nodes = {}
  for _, node in tree:iter_nodes() do
    table.insert(nodes, node)
  end
  local function command_for(node)
    local spec = adapter.build_spec({ tree = { data = function() return node:data() end }, extra_args = { '--no-run' } })
    return spec.command, spec.cwd
  end
  local command, cwd = command_for(nodes[3])
  eq({ 'kargo', 'test', '--message-format', 'json', 'com.example.app.MathTest.adds', '--no-run' }, command)
  eq(project, cwd)
  eq({ 'kargo', 'test', '--message-format', 'json', 'com.example.app.MathTest', '--no-run' }, (command_for(nodes[2])))
  eq({ 'kargo', 'test', '--message-format', 'json', 'com.example.app', '--no-run' }, (command_for(nodes[1])))
end)

test('results report each test and roll suites and files up from their tests', function()
  local tree = adapter.discover_positions(test_file)
  local output = vim.fn.tempname()
  vim.fn.writefile(vim.split(jsonl(
    { reason = 'test-finished', suite = 'com.example.app.MathTest', name = 'adds', outcome = 'passed' },
    { reason = 'test-finished', suite = 'com.example.app.MathTest', name = 'divides', outcome = 'failed', message = 'expected 2', details = 'java.lang.AssertionError\n\tat com.example.app.MathTest.divides(MathTest.kt:8)' },
    { reason = 'test-summary', target = 'jvm', passed = 1, failed = 1, ignored = 0, success = false }
  ), '\n'), output)
  local results = adapter.results({ context = { path = test_file } }, { output = output, code = 101 }, tree)
  eq('passed', results[test_file .. '::com.example.app.MathTest::adds'].status)
  eq('failed', results[test_file .. '::com.example.app.MathTest::divides'].status)
  eq('expected 2', results[test_file .. '::com.example.app.MathTest::divides'].short)
  eq(7, results[test_file .. '::com.example.app.MathTest::divides'].errors[1].line)
  eq('failed', results[test_file .. '::com.example.app.MathTest'].status)
  eq('failed', results[test_file].status)
end)

test('a run that printed no results gives none', function()
  local output = vim.fn.tempname()
  vim.fn.writefile({ 'error: no tests found' }, output)
  eq({}, adapter.results({ context = { path = test_file } }, { output = output, code = 101 }, adapter.discover_positions(test_file)))
end)

local dap_stub = { adapters = {}, configurations = { kotlin = { { type = 'lldb', name = 'other' } } } }
package.preload['dap'] = function()
  return dap_stub
end

test('dap setup adds the kargo adapter and configurations once', function()
  local dap = require('kargo.dap')
  dap.setup()
  dap.setup()
  eq('function', type(dap_stub.adapters.kargo))
  local types = vim.tbl_map(function(c)
    return c.type
  end, dap_stub.configurations.kotlin)
  eq({ 'lldb', 'kargo', 'kargo', 'kargo' }, types)
  local adapter_config
  dap_stub.adapters.kargo(function(config) adapter_config = config end, { cwd = project })
  eq({ type = 'executable', command = 'kargo', args = { 'dap' }, options = { cwd = project } }, adapter_config)
end)

test('the language server config starts kargo lsp for Kotlin and TOML', function()
  local config = dofile(root .. '/lsp/kargo.lua')
  eq({ 'kargo', 'lsp' }, config.cmd)
  eq({ 'kotlin', 'toml' }, config.filetypes)
  eq({ 'kargo.lock', 'kargo.toml' }, config.root_markers)
end)

test(':Kargo completes subcommands only in the subcommand position', function()
  local kargo = require('kargo')
  eq({ 'build', 'check', 'test' }, vim.tbl_filter(function(name)
    return name == 'build' or name == 'check' or name == 'test'
  end, kargo.complete('', 'Kargo ')))
  eq({ 'build' }, kargo.complete('b', 'Kargo b'))
  eq({}, kargo.complete('', 'Kargo build '))
end)

test('setup points the language server at the same kargo executable', function()
  require('kargo').setup({ cmd = '/opt/kargo/bin/kargo' })
  eq({ '/opt/kargo/bin/kargo', 'lsp' }, vim.lsp.config.kargo.cmd)
  require('kargo').setup({ cmd = 'kargo' })
  eq({ 'kargo', 'lsp' }, vim.lsp.config.kargo.cmd)
end)

vim.fn.delete(fixture_dir, 'rf')

if failures > 0 then
  print(('\n%d test(s) failed'):format(failures))
  vim.cmd('cquit 1')
end
print('\nall tests passed')
