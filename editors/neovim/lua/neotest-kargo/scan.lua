--- Finds the test classes and test functions of a Kotlin file from its lines, without a parser or a
--- compiler: a `class` line starts a suite, and a `fun` below a `@Test` annotation is a test.
local M = {}

--- The package and the tests of a Kotlin file. A suite is `{ name, fqn, row }`, where `fqn` is the
--- package-qualified name `kargo test` reports. A test is `{ name, row, suite }`, where `suite` is
--- the last class declared above it (nil for a function outside any class). Rows are 1-based.
---@param lines string[]
---@return { package: string, suites: table[], tests: table[] }
function M.scan(lines)
  local package = ''
  local suites, tests = {}, {}
  local current
  local annotated = false
  for row, line in ipairs(lines) do
    package = line:match('^%s*package%s+([%w_.]+)') or package

    local class = line:match('^%s*[%w%s]-%f[%w]class%s+([%w_]+)')
    if class then
      current = { name = class, row = row, fqn = package ~= '' and (package .. '.' .. class) or class }
      table.insert(suites, current)
    end

    if line:find('@Test%f[%W]') or line:find('@ParameterizedTest%f[%W]') then
      annotated = true
    end
    local name = line:match('%f[%w]fun%s+`([^`]+)`') or line:match('%f[%w]fun%s+([%w_]+)%s*%(')
    if name then
      if annotated then
        table.insert(tests, { name = name, row = row, suite = current })
      end
      annotated = false
    end
  end
  return { package = package, suites = suites, tests = tests }
end

return M
