--- Reads the JSON lines Kargo prints with `--message-format json`: compiler diagnostics, test
--- results and summaries. Pure functions, so the tests run without a Kargo binary or a project.
local M = {}

--- Decodes the JSON objects in `text`, one per line. Lines that aren't objects with a `reason`,
--- such as the program's own output, are skipped.
---@param text string
---@return table[]
function M.decode(text)
  local decoded = {}
  for line in vim.gsplit(text, '\n', { plain = true, trimempty = true }) do
    local ok, value = pcall(vim.json.decode, line)
    if ok and type(value) == 'table' and value.reason then
      table.insert(decoded, value)
    end
  end
  return decoded
end

local severities = { error = 'E', exception = 'E', warning = 'W', info = 'I', note = 'I' }

--- A JSON field's value, or `default` when it is absent or `null`.
---@param value any
---@param default any
---@return any
local function field(value, default)
  if value == nil or value == vim.NIL then
    return default
  end
  return value
end

--- The quickfix entries of the compiler diagnostics. A diagnostic without a file, such as a
--- linker message, becomes a plain entry with its text.
---@param messages table[]
---@return table[]
function M.quickfix(messages)
  local items = {}
  for _, message in ipairs(messages) do
    if message.reason == 'compiler-message' then
      table.insert(items, {
        filename = field(message.file, ''),
        lnum = field(message.line, 0),
        col = field(message.column, 0),
        type = severities[message.severity] or 'E',
        text = message.message,
      })
    end
  end
  return items
end

--- The compiler diagnostics that have a file and a line, for `vim.diagnostic`.
---@param messages table[]
---@return table<string, table[]> diagnostics by absolute file path
function M.diagnostics(messages)
  local by_file = {}
  local severity = { E = vim.diagnostic.severity.ERROR, W = vim.diagnostic.severity.WARN, I = vim.diagnostic.severity.INFO }
  for _, item in ipairs(M.quickfix(messages)) do
    if item.filename ~= '' and item.lnum > 0 then
      by_file[item.filename] = by_file[item.filename] or {}
      table.insert(by_file[item.filename], {
        lnum = item.lnum - 1,
        col = math.max(item.col - 1, 0),
        severity = severity[item.type],
        message = item.text,
        source = 'kargo',
      })
    end
  end
  return by_file
end

--- The tests that finished, with their outcome (`passed`, `failed` or `ignored`) and, for a
--- failure, the message and stack trace.
---@param messages table[]
---@return table[]
function M.test_results(messages)
  local results = {}
  for _, message in ipairs(messages) do
    if message.reason == 'test-finished' then
      table.insert(results, {
        suite = field(message.suite, nil),
        name = message.name,
        display = field(message['display-name'], nil),
        outcome = message.outcome,
        message = field(message.message, nil),
        details = field(message.details, nil),
      })
    end
  end
  return results
end

--- The counts of the `test-summary` messages, added up over the targets.
---@param messages table[]
---@return { passed: integer, failed: integer, ignored: integer, success: boolean }
function M.summary(messages)
  local total = { passed = 0, failed = 0, ignored = 0, success = true }
  for _, message in ipairs(messages) do
    if message.reason == 'test-summary' then
      total.passed = total.passed + (message.passed or 0)
      total.failed = total.failed + (message.failed or 0)
      total.ignored = total.ignored + (message.ignored or 0)
      total.success = total.success and message.success == true
    end
  end
  return total
end

--- Whether the `build-finished` message says the build succeeded, or nil without one.
---@param messages table[]
---@return boolean?
function M.build_success(messages)
  for _, message in ipairs(messages) do
    if message.reason == 'build-finished' then
      return message.success == true
    end
  end
  return nil
end

return M
