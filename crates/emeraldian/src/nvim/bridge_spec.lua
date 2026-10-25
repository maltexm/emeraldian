-- Spec for bridge.lua's pure parts (link parsing, anchor lookup).
-- Run: nvim --headless --clean -l crates/emeraldian/src/nvim/bridge_spec.lua
-- Exits non-zero and prints every failed case; `tests/nvim_bridge.rs` runs it.

local here = debug.getinfo(1, "S").source:sub(2):match("^(.*)/[^/]*$") or "."
local M = dofile(here .. "/bridge.lua")

local failures = {}
local count = 0

local function eq(name, got, want)
  count = count + 1
  if not vim.deep_equal(got, want) then
    failures[#failures + 1] = string.format(
      "%s: got %s, want %s", name, vim.inspect(got), vim.inspect(want)
    )
  end
end

-- The columns are 0-based byte offsets, as nvim_win_get_cursor reports them.
local function link(line, col)
  local l = M.link_at(line, col)
  if not l then
    return nil
  end
  return { l.kind, l.note, l.heading, l.block }
end

-- wikilinks
eq("wiki plain", link("see [[Alpha]] here", 7), { "wiki", "Alpha" })
eq("wiki on opening bracket", link("see [[Alpha]] here", 4), { "wiki", "Alpha" })
eq("wiki alias dropped", link("[[Alpha|alias]]", 3), { "wiki", "Alpha" })
eq("wiki alias, cursor in alias", link("[[Alpha|alias]]", 10), { "wiki", "Alpha" })
eq("wiki folder and heading", link("[[Folder/Alpha#Heading]]", 5),
  { "wiki", "Folder/Alpha", "Heading" })
eq("wiki block", link("[[Alpha#^blk1]]", 3), { "wiki", "Alpha", nil, "blk1" })
eq("wiki heading with alias", link("[[Alpha#Sec|text]]", 3), { "wiki", "Alpha", "Sec" })
eq("wiki same-note heading", link("[[#Sec]]", 3), { "wiki", "", "Sec" })
eq("wiki nested heading takes the last", link("[[Alpha#One#Two]]", 3),
  { "wiki", "Alpha", "Two" })
eq("embed is a link too", link("![[Alpha]]", 4), { "wiki", "Alpha" })
eq("wiki, no link before the brackets", link("see [[Alpha]] here", 1), nil)

-- the `]]` edge: both closing brackets are part of the link, the next cell is not
eq("edge: first closing bracket", link("[[Alpha]]", 7), { "wiki", "Alpha" })
eq("edge: last closing bracket", link("[[Alpha]]", 8), { "wiki", "Alpha" })
eq("edge: just past the link", link("[[Alpha]] x", 9), nil)

-- two links on one line: the one under the cursor
eq("two links, first", link("[[A]] and [[B]]", 2), { "wiki", "A" })
eq("two links, second", link("[[A]] and [[B]]", 12), { "wiki", "B" })
eq("two links, between", link("[[A]] and [[B]]", 7), nil)

-- markdown links
eq("md relative", link("[text](../Beta.md)", 3), { "md", "../Beta.md" })
eq("md on the url", link("[text](../Beta.md)", 12), { "md", "../Beta.md" })
eq("md percent-encoded", link("[text](Beta%20Two.md)", 3), { "md", "Beta Two.md" })
eq("md anchor", link("[t](Beta.md#Sec)", 2), { "md", "Beta.md", "Sec" })
eq("md block anchor", link("[t](Beta.md#^b1)", 2), { "md", "Beta.md", nil, "b1" })
eq("md unwrapped spaces", link("[t](Note C.md)", 2), { "md", "Note C.md" })
eq("md spaces and title", link('[t](Note C.md "a title")', 2), { "md", "Note C.md" })
eq("md spaces and fragment", link("[t](Note C.md#Some Heading)", 2), { "md", "Note C.md", "Some Heading" })
eq("md angle brackets", link("[t](<Beta Two.md>)", 2), { "md", "Beta Two.md" })
eq("md external is no link", link("[ext](https://x)", 3), nil)
eq("md mailto is no link", link("[m](mailto:a@b.c)", 3), nil)
eq("md non-note file is no link", link("[img](pic.png)", 3), nil)
eq("md image is no note", link("![img](pic.png)", 3), nil)

-- a wikilink and a markdown link side by side
eq("mixed, wiki", link("[[A]] [b](B.md)", 2), { "wiki", "A" })
eq("mixed, md", link("[[A]] [b](B.md)", 9), { "md", "B.md" })

-- spans are reported so the caller can tell where the link is
local l = M.link_at("see [[Alpha]] here", 7)
eq("span start", l and l.s, 4)
eq("span end", l and l.e, 12)

-- anchors
local lines = {
  "# Title",
  "text",
  "## Sub heading ##",
  "- item ^blk1",
  "### Sub Heading",
  "para ^blk2  ",
}
eq("heading found", M.find_anchor(lines, "heading", "Title"), 1)
eq("heading is case-insensitive", M.find_anchor(lines, "heading", "sub heading"), 3)
eq("closing hashes ignored", M.find_anchor(lines, "heading", "Sub heading"), 3)
eq("heading missing", M.find_anchor(lines, "heading", "Nope"), nil)
eq("heading ignores plain text", M.find_anchor(lines, "heading", "text"), nil)
eq("block found", M.find_anchor(lines, "block", "blk1"), 4)
eq("block with trailing blanks", M.find_anchor(lines, "block", "blk2"), 6)
eq("block missing", M.find_anchor(lines, "block", "blk3"), nil)
eq("block must end the line", M.find_anchor({ "x ^blk1 more" }, "block", "blk1"), nil)

-- the action names the Rust side passes in are the only ones the command takes
eq("action check", M.is_action({ "switcher", "search" }, "switcher"), true)
eq("action check, unknown", M.is_action({ "switcher", "search" }, "nope"), false)

-- `]` / `[`: the next or previous link in the buffer, counted from the cursor.
-- 1-based line, 0-based column, as nvim reports them.
local function jump(lines, lnum, col, delta)
  local pos = M.next_link(lines, lnum, col, delta)
  return pos and { pos[1], pos[2] } or nil
end

local note = {
  "see [[Alpha]] first",
  "plain text",
  "then [there](Beta.md) and [[Gamma]]",
}
eq("next from the top", jump(note, 1, 0, 1), { 1, 4 })
eq("next skips the link under the cursor", jump(note, 1, 4, 1), { 3, 5 })
eq("next wraps to the first", jump(note, 3, 26, 1), { 1, 4 })
eq("prev from the top wraps to the last", jump(note, 1, 0, -1), { 3, 26 })
eq("prev steps back", jump(note, 3, 26, -1), { 3, 5 })
eq("count jumps that many links", jump(note, 1, 0, 3), { 3, 26 })
eq("no link anywhere", jump({ "nothing here" }, 1, 0, 1), nil)

-- Backward from prose chooses the nearest preceding link; inside either
-- bracket, label or destination counts as being on that link in both directions.
local prose = { "[[A]]", "plain text", "[[B]]", "plain text", "[[C]]", "tail" }
eq("prev from prose", jump(prose, 2, 0, -1), { 1, 0 })
eq("prev from tail", jump(prose, 6, 0, -1), { 5, 0 })
eq("prev from prose counted", jump(prose, 6, 0, -2), { 3, 0 })
eq("prev from prose wraps", jump(prose, 2, 0, -2), { 5, 0 })
eq("prev on link start skips it", jump(prose, 3, 0, -1), { 1, 0 })
for col = 0, 4 do
  eq("prev inside link byte " .. col, jump(prose, 3, col, -1), { 1, 0 })
  eq("next inside link byte " .. col, jump(prose, 3, col, 1), { 5, 0 })
end
eq("prev just past link", jump({ "[[A]] x [[B]]" }, 1, 5, -1), { 1, 0 })
eq("prev in same-line gap", jump({ "[[A]] x [[B]]" }, 1, 7, -1), { 1, 0 })
eq("prev inside same-line second link", jump({ "[[A]] x [[B]]" }, 1, 10, -1), { 1, 0 })
eq("prev full count on link", jump(prose, 3, 2, -3), { 3, 0 })
eq("prev full count from prose", jump(prose, 4, 0, -3), { 5, 0 })
eq("next full count from prose", jump(prose, 4, 0, 3), { 3, 0 })

-- A fenced link is not a link. A bare URL is: reading mode lists those too,
-- and `]` has to land on the same ones.
local fenced = {
  "see [[Alpha]]",
  "```",
  "[[Hidden]]",
  "```",
  "then https://example.org/x and [[Gamma]]",
}
eq("fence is not a link", jump(fenced, 1, 0, 1), { 1, 4 })
eq("fence skipped, bare url is a link", jump(fenced, 1, 4, 1), { 5, 5 })
eq("bare url then the wiki after it", jump(fenced, 5, 5, 1), { 5, 31 })

for _, char in ipairs({ "`", "~" }) do
  local outer = char:rep(4)
  local cases = {
    { char:rep(3), "shorter" },
    { outer .. "not-a-close", "info string" },
    { (char == "`" and "~" or "`"):rep(4), "other character" },
  }
  for _, case in ipairs(cases) do
    local block = { "[[A]]", outer .. "markdown", case[1], "[[Hidden]]", outer, "[[B]]" }
    eq(char .. " fence ignores " .. case[2], jump(block, 1, 0, 1), { 6, 0 })
  end
  eq(char .. " longer closer", jump({ "[[A]]", char:rep(3), "[[Hidden]]", "  " .. outer .. "  ", "[[B]]" }, 1, 0, 1), { 5, 0 })
  eq(char .. " unclosed fence", jump({ "[[A]]", outer, "[[Hidden]]" }, 1, 0, 1), { 1, 0 })
  eq(char .. " two characters are prose", jump({ "[[A]]", char:rep(2), "[[B]]" }, 1, 0, 1), { 3, 0 })
end

-- Attachment stays broad, but raw diagram buffers have no Markdown follow.
local root = vim.fn.tempname()
vim.fn.mkdir(root, "p")
M.setup(nil, { root = root })
for _, ext in ipairs({ "mmd", "mermaid", "excalidraw" }) do
  vim.cmd.edit(root .. "/flow." .. ext)
  vim.api.nvim_buf_set_lines(0, 0, -1, false, { "flowchart LR", "A[[Old]] --> B" })
  vim.api.nvim_win_set_cursor(0, { 2, 2 })
  for _, key in ipairs({ "]", "[", "<CR>", "gf", "<C-]>" }) do
    local map = vim.fn.maparg(key, "n", false, true)
    eq(ext .. " attached " .. key, type(map.callback), "function")
    eq(ext .. " buffer-local " .. key, map.buffer > 0, true)
    if key == "]" or key == "[" then
      eq(ext .. " nowait " .. key, map.nowait, 1)
    end
  end
  eq(ext .. " does not follow diagram data", M.follow(), false)
  vim.bo.modified = false
end
vim.cmd.enew()
vim.fn.delete(root, "d")

if #failures > 0 then
  io.stderr:write(table.concat(failures, "\n"), "\n")
  io.stderr:write(string.format("%d of %d cases failed\n", #failures, count))
  os.exit(1)
end
print(string.format("bridge_spec: %d cases passed", count))
