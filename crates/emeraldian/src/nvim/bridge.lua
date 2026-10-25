-- emeraldian bridge: injected into the embedded nvim by `nvim_exec_lua`.
--
-- Independent of the user's profile. It only talks to emeraldian through RPC
-- notifications named `emeraldian.<event>` (see `bridge.rs` for what each
-- carries) and one request, `emeraldian.notes`.
--
-- Loaded two ways:
--   * injected: `nvim_exec_lua(code, { channel, opts })`, so `...` is set and
--     the bridge installs itself;
--   * `dofile` from `bridge_spec.lua`: no arguments, nothing is installed, only
--     the pure functions (`link_at`, `find_anchor`, `is_action`) are used.

local chan, opts = ...

local M = {}

-- ---------------------------------------------------------------------------
-- Links
-- ---------------------------------------------------------------------------

local function percent_decode(text)
  return (text:gsub("%%(%x%x)", function(hex)
    return string.char(tonumber(hex, 16))
  end))
end

--- `Sec`, `^blk`, `One#Two` (the part after the first `#`) -> heading, block.
--- Nested headings (`Note#One#Two`) point at the last one.
local function split_fragment(fragment)
  if not fragment or fragment == "" then
    return nil, nil
  end
  local last = fragment:match("([^#]*)$")
  if last:sub(1, 1) == "^" then
    local block = last:sub(2)
    return nil, block ~= "" and block or nil
  end
  return last ~= "" and last or nil, nil
end

--- All wikilinks and local markdown links in `line`, with 0-based inclusive
--- byte spans. External links and links to non-note files are not links here.
local function scan(line)
  local found = {}

  local init = 1
  while true do
    local s, e, inner = line:find("%[%[([^%[%]]+)%]%]", init)
    if not s then
      break
    end
    -- `[[A\|b]]`: the backslash escapes the pipe inside tables.
    local target = inner:match("^([^|]*)"):gsub("\\$", "")
    local note, fragment = target:match("^([^#]*)#(.*)$")
    if not note then
      note, fragment = target, nil
    end
    local heading, block = split_fragment(fragment)
    found[#found + 1] = {
      kind = "wiki",
      note = vim.trim(note),
      heading = heading,
      block = block,
      s = s - 1,
      e = e - 1,
    }
    init = e + 1
  end

  init = 1
  while true do
    local s, e, _, url = line:find("%[([^%]]*)%]%(([^)]*)%)", init)
    if not s then
      break
    end
    init = e + 1
    url = vim.trim(url)
    local path
    if url:sub(1, 1) == "<" then
      path = url:match("^<([^>]*)>")
    else
      -- Vault paths commonly contain spaces. Only a quoted Markdown title
      -- terminates an unwrapped destination; ordinary spaces belong to it.
      path = url:match('^(.-)%s+".*"$') or url:match("^(.-)%s+'.*'$") or url
    end
    -- `https:`, `mailto:`, ... are somebody else's business.
    if path and not path:match("^%a[%w+.-]*:") then
      local file, fragment = path:match("^([^#]*)#(.*)$")
      if not file then
        file, fragment = path, nil
      end
      file = percent_decode(file)
      local same_note = file == "" and fragment ~= nil
      if same_note or file:lower():match("%.md$") then
        local heading, block = split_fragment(fragment and percent_decode(fragment))
        found[#found + 1] = {
          kind = "md",
          note = file,
          heading = heading,
          block = block,
          s = s - 1,
          e = e - 1,
        }
      end
    end
  end

  return found
end

--- Bare `https://...` / `http://...` spans `scan` ignores. Reading mode lists
--- those as links, so the jump has to land on them too. 0-based inclusive.
local function bare_urls(line)
  local found = {}
  local init = 1
  while true do
    local s, e = line:find("https?://%S+", init)
    if not s then
      break
    end
    -- A trailing mark is punctuation, not part of the address.
    local bare = line:sub(s, e):gsub("[%.,;:!?)]+$", "")
    found[#found + 1] = { s = s - 1, e = s + #bare - 2 }
    init = e + 1
  end
  return found
end

--- The link under 0-based byte column `col` of `line`, or nil. Both closing
--- brackets belong to a wikilink; the cell after them does not.
function M.link_at(line, col)
  local best
  for _, link in ipairs(scan(line)) do
    if col >= link.s and col <= link.e then
      if not best or (link.e - link.s) < (best.e - best.s) then
        best = link
      end
    end
  end
  return best
end

--- Every link in `lines`, in reading order, as `{ line, col }` (1-based line,
--- 0-based column of the link's first byte). A fenced block is not a link, the
--- same rule the reading view uses; a bare URL is.
local function all_links(lines)
  local found = {}
  local fence
  for lnum, line in ipairs(lines) do
    local marker = line:match("^%s*(```+)") or line:match("^%s*(~~~+)")
    if marker then
      if fence == nil then
        fence = marker
      elseif fence:sub(1, 1) == marker:sub(1, 1)
        and #marker >= #fence
        and vim.trim(line):sub(#marker + 1):match("^%s*$") then
        fence = nil
      end
    elseif not fence then
      for _, link in ipairs(scan(line)) do
        found[#found + 1] = { lnum, link.s, link.e }
      end
      for _, url in ipairs(bare_urls(line)) do
        found[#found + 1] = { lnum, url.s, url.e }
      end
    end
  end
  -- `scan` lists wikilinks before markdown links, not left to right.
  table.sort(found, function(a, b)
    return a[1] < b[1] or (a[1] == b[1] and a[2] < b[2])
  end)
  return found
end

--- Where `]` / `[` land. `delta` is signed and counts links: positive forward,
--- negative back. A link the cursor is already on does not count as the next
--- one. Wraps. nil when the buffer has no link.
function M.next_link(lines, lnum, col, delta)
  local links = all_links(lines)
  local n = #links
  if n == 0 or delta == 0 then
    return nil
  end
  local before, on = 0, nil
  for i, pos in ipairs(links) do
    if pos[1] < lnum or (pos[1] == lnum and pos[2] < col) then
      before = i
    end
    if pos[1] == lnum and pos[2] <= col and col <= pos[3] then
      on = i
    end
  end
  -- Inside a link (including both delimiters), skip that link. In prose,
  -- backward starts at the nearest preceding link, not the one before it.
  local here = on or (delta > 0 and before or before + 1)
  local pos = links[(here - 1 + delta) % n + 1]
  return { pos[1], pos[2] }
end

--- 1-based line number of a heading (`kind = "heading"`, case-insensitive,
--- closing `#`s ignored) or of a block id (`kind = "block"`, `^id` ending the
--- line), or nil.
function M.find_anchor(lines, kind, text)
  if kind == "heading" then
    local want = text:lower()
    for lnum, line in ipairs(lines) do
      local hashes, rest = line:match("^(#+)%s+(.-)%s*$")
      if hashes and #hashes <= 6 then
        rest = rest:gsub("%s+#+$", "")
        if rest:lower() == want then
          return lnum
        end
      end
    end
  elseif kind == "block" then
    local pattern = "%^" .. text:gsub("%p", "%%%0") .. "%s*$"
    for lnum, line in ipairs(lines) do
      local s = line:find(pattern)
      if s and (s == 1 or line:sub(s - 1, s - 1):match("%s")) then
        return lnum
      end
    end
  end
  return nil
end

function M.is_action(actions, name)
  for _, action in ipairs(actions) do
    if action == name then
      return true
    end
  end
  return false
end

-- ---------------------------------------------------------------------------
-- Everything below needs a running emeraldian on the other end of `chan`.
-- ---------------------------------------------------------------------------

local root = ""

local function send(event, ...)
  if chan then
    pcall(vim.rpcnotify, chan, "emeraldian." .. event, ...)
  end
end

local function in_vault(name)
  return name ~= "" and vim.startswith(name, root .. "/")
end

--- A real file buffer inside the vault: the only kind emeraldian tracks.
local function note_buf(buf)
  return vim.api.nvim_buf_is_valid(buf)
    and vim.bo[buf].buftype == ""
    and in_vault(vim.api.nvim_buf_get_name(buf))
end

--- Whether quitting the current window would end nvim.
function M.last_window()
  local wins = vim.tbl_filter(function(win)
    return vim.api.nvim_win_get_config(win).relative == ""
  end, vim.api.nvim_tabpage_list_wins(0))
  return #wins == 1 and #vim.api.nvim_list_tabpages() == 1
end

--- Standalone diagram source is data, not Markdown. In particular Mermaid's
--- `A[[label]]` is a node, not a wikilink. `.excalidraw.md` remains Markdown,
--- matching the core index's distinction between wrapped and raw drawings.
local function markdown_buf(buf)
  return vim.api.nvim_buf_get_name(buf):lower():match("%.md$") ~= nil
end

--- Follows the link under the cursor. false when there is none, so the caller
--- can fall back to the key's usual meaning.
function M.follow()
  local buf = vim.api.nvim_get_current_buf()
  if not markdown_buf(buf) then
    return false
  end
  local cursor = vim.api.nvim_win_get_cursor(0)
  local link = M.link_at(vim.api.nvim_get_current_line(), cursor[2])
  if not link then
    return false
  end
  local source = vim.api.nvim_buf_get_name(buf)
  local target = { note = link.note, heading = link.heading, block = link.block }
  if link.note == "" then
    -- `[[#Heading]]`: a heading of this very note.
    target.note = in_vault(source) and source:sub(#root + 2) or source
  elseif link.kind == "md" then
    -- Markdown links are relative to the note they are written in.
    target.source = source
  end
  send("follow", target)
  return true
end

--- Moves the cursor of the current window to a heading or block of the buffer.
function M.goto_anchor(kind, text)
  local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
  local lnum = M.find_anchor(lines, kind, text)
  if lnum then
    vim.api.nvim_win_set_cursor(0, { lnum, 0 })
    vim.cmd("normal! zt")
  end
end

--- The jump emeraldian queues in the key stream, right behind the buffer switch:
--- `text` is hex-encoded, so that nothing in a heading (quotes, brackets, `<`)
--- can be mistaken for syntax. An API call carrying it would not do: nvim
--- consumes typeahead before queued API calls.
function M.goto_hex(kind, hex)
  local text = hex:gsub("%x%x", function(byte)
    return string.char(tonumber(byte, 16))
  end)
  M.goto_anchor(kind, text)
end

--- Back to the reading view. `write` is `:wq`/`:x`: save first, and stay if
--- that fails. With auto-save any quit saves, as everything else does.
function M.leave(write)
  if write then
    local ok, err = pcall(vim.cmd, "update")
    if not ok then
      vim.api.nvim_echo({ { tostring(err), "ErrorMsg" } }, true, {})
      return
    end
  elseif opts and opts.auto_save then
    M.update(vim.api.nvim_get_current_buf())
  end
  send("leave")
end

--- Note titles and aliases from emeraldian, for completion. Blocks until
--- emeraldian answers, so call it from a callback, never from inside an RPC
--- request emeraldian is itself waiting on.
function M.notes()
  local ok, names = pcall(vim.rpcrequest, chan, "emeraldian.notes")
  return ok and names or {}
end

-- Calls emeraldian makes on the buffers it tracks -----------------------------
--
-- Each runs inside one RPC request, so nothing can be typed between its steps.

--- Writes the buffer. Errors are nvim's own (read-only, a file that changed on
--- disk and needs an answer) and travel back to the caller.
function M.save(buf)
  vim.api.nvim_buf_call(buf, function()
    vim.cmd("silent update")
  end)
end

--- The note behind `buf` was moved to `name` on disk. The buffer follows it.
---
--- `:file` marks a buffer "not edited", and a write then stops at E13 because
--- the file it is named for exists. A clean buffer is read again, which clears
--- that; one with unsaved edits is written, so they are kept in the new file.
function M.rename_buffer(buf, name)
  local old = vim.api.nvim_buf_get_name(buf)
  local modified = vim.bo[buf].modified
  vim.api.nvim_buf_set_name(buf, name)
  -- The old name stays behind as an unlisted buffer. A jump back to it would
  -- open a note that is no longer there, and saving that would bring it back.
  for _, other in ipairs(vim.api.nvim_list_bufs()) do
    if other ~= buf and vim.api.nvim_buf_get_name(other) == old then
      pcall(vim.api.nvim_buf_delete, other, { force = true })
    end
  end
  vim.api.nvim_buf_call(buf, function()
    vim.cmd(modified and "silent write!" or "silent edit!")
  end)
end

--- Replaces the buffer's text and writes it. `!` because the file on disk may
--- have been rewritten by the very operation this belongs to, which nvim would
--- otherwise ask about.
function M.set_lines(buf, lines)
  if not vim.deep_equal(vim.api.nvim_buf_get_lines(buf, 0, -1, false), lines) then
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  end
  vim.api.nvim_buf_call(buf, function()
    vim.cmd("silent update!")
  end)
end

--- Reloads every buffer without unsaved edits whose file changed on disk. A
--- buffer with edits is left to nvim, which asks the user what to do; that
--- question must not come up inside this call, where nobody can answer it.
function M.checktime()
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and note_buf(buf) and not vim.bo[buf].modified then
      vim.api.nvim_buf_call(buf, function()
        vim.cmd("setlocal autoread")
        pcall(vim.cmd, "silent! checktime " .. buf)
        vim.cmd("setlocal autoread<")
      end)
    end
  end
end

--- Removes the buffer; without `force`, a buffer with unsaved edits stays.
function M.close_buffer(buf, force)
  -- Not left to `nvim_buf_delete`, which reports E89 on the screen and waits
  -- for a key, with nobody to press it.
  if not force and vim.bo[buf].modified then
    error("the note has unsaved changes", 0)
  end
  -- Forced means the edits are given up. Leaving them marked would have
  -- auto-save write them as the buffer is left (`BufLeave`), which for a note
  -- that was just deleted brings it back.
  if force then
    vim.bo[buf].modified = false
  end
  vim.api.nvim_buf_delete(buf, { force = force })
end

--- The buffer nvim holds for the file `name`, or nil. Asked of nvim every time:
--- a buffer can be opened (`:edit`), wiped and opened again behind emeraldian's
--- back, and a handle remembered from earlier then points at nothing or at
--- another buffer.
function M.buffer_of(name)
  local function real(path)
    local resolved = vim.uv.fs_realpath(path)
    if resolved then
      return resolved
    end
    -- A file that is gone (a note just moved away): its folder still resolves.
    local dir = vim.uv.fs_realpath(vim.fs.dirname(path))
    return dir and (dir .. "/" .. vim.fs.basename(path)) or path
  end
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_get_name(buf) == name then
      return buf
    end
  end
  local want = real(name)
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local have = vim.api.nvim_buf_get_name(buf)
    if have ~= "" and real(have) == want then
      return buf
    end
  end
  return nil
end

--- The names of the notes whose buffers have unsaved edits.
function M.modified_notes()
  local names = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and note_buf(buf) and vim.bo[buf].modified then
      names[#names + 1] = vim.api.nvim_buf_get_name(buf)
    end
  end
  return names
end

--- The names of all buffers that have one.
function M.buffer_names()
  local names = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local name = vim.api.nvim_buf_get_name(buf)
    if name ~= "" then
      names[#names + 1] = name
    end
  end
  return names
end

-- Auto-save ------------------------------------------------------------------

local timers = {}

function M.update(buf)
  if
    note_buf(buf)
    and vim.bo[buf].modified
    and vim.bo[buf].modifiable
    and not vim.bo[buf].readonly
  then
    vim.api.nvim_buf_call(buf, function()
      vim.cmd("silent! update")
    end)
  end
end

local function stop_timer(buf)
  local timer = timers[buf]
  if timer then
    timer:stop()
    timer:close()
    timers[buf] = nil
  end
end

local function schedule_update(buf)
  local timer = timers[buf]
  if not timer then
    timer = vim.uv.new_timer()
    timers[buf] = timer
  end
  timer:stop()
  timer:start(
    opts.debounce_ms or 1000,
    0,
    vim.schedule_wrap(function()
      if not vim.api.nvim_buf_is_valid(buf) then
        stop_timer(buf)
        return
      end
      -- Not while a command line or a prompt is open: try again shortly.
      if vim.api.nvim_get_mode().mode:match("^[cr!]") then
        schedule_update(buf)
        return
      end
      M.update(buf)
    end)
  )
end

-- Buffer-local maps ----------------------------------------------------------

--- The private name an expression map of `lhs` is kept under.
local function kept_name(lhs)
  return "<Plug>(emeraldian-previous-" .. lhs:gsub("%W", "_") .. ")"
end

local function termcodes(keys)
  return vim.api.nvim_replace_termcodes(keys, true, false, true)
end

-- A replay goes back through the typeahead, so what it produces can reach the
-- bridge's own map again. For a map nvim expands itself that is bounded
-- (`maxmapdepth`), and a result that starts with the map's own lhs is not
-- remapped at that point. Every replay here is a fresh `nvim_feedkeys`, which
-- resets both, so the bridge keeps the two rules itself:
--   * `armed[lhs]`: the replay produced a remappable result that starts with
--     `lhs`. The next run of the bridge's map for `lhs` is that leading `lhs`,
--     which is not remapped: the key does its default.
--   * `depth`: replays since nvim last looked for input. Past `maxmapdepth`
--     the chain is cut with the error nvim gives for it (E223).
-- Both are reset when the event loop runs again, i.e. once the keys are done.
local replay = { depth = 0, armed = {} }
local reset_pending = false

local function reset_soon()
  if reset_pending then
    return
  end
  reset_pending = true
  vim.schedule(function()
    reset_pending = false
    replay.depth = 0
    replay.armed = {}
  end)
end

--- Counts one replay; false (after saying why) once the chain is too deep.
local function replay_allowed()
  replay.depth = replay.depth + 1
  reset_soon()
  if replay.depth > vim.o.maxmapdepth then
    vim.notify("E223: Recursive mapping", vim.log.levels.ERROR)
    return false
  end
  return true
end

--- Notes what a replay for `lhs` produced (`keys`, as typed: keycodes
--- translated): when it is remapped and starts with `lhs`, the bridge's map
--- runs next for that leading `lhs`.
local function note_replay_keys(lhs, remapped, keys)
  if remapped and type(keys) == "string" and vim.startswith(keys, termcodes(lhs)) then
    replay.armed[lhs] = true
    reset_soon()
  end
end

--- The key a kept expression string belongs to, by an id without `<...>`,
--- which nvim would translate inside the string.
local kept_keys = {}

--- Called by a kept expression string (see `keep_expr_map`) with its result,
--- which is passed on unchanged.
function M.expr_result(id, remapped, result)
  local lhs = kept_keys[id]
  if lhs then
    note_replay_keys(lhs, remapped == 1, type(result) == "string" and result or tostring(result))
  end
  return result
end

--- Keeps the user's expression map of `lhs` (`previous`, from `maparg`) under a
--- private `<Plug>` name in `buf`. Its result cannot be had by calling it or
--- evaluating its `rhs` here: whether keycodes in the result are translated,
--- whether it is remapped, `v:count` and `silent` all belong to the map, and
--- nvim applies them to a map it runs itself. The result is only looked at on
--- its way out, to arm the self-lhs rule above. Sets `previous.kept` on success.
local function keep_expr_map(buf, lhs, previous)
  local name = kept_name(lhs)
  local remapped = previous.noremap ~= 1 and previous.script ~= 1
  if previous.callback then
    local callback = previous.callback
    local translated = previous.replace_keycodes == 1
    previous.kept = pcall(vim.api.nvim_buf_set_keymap, buf, "n", name, "", {
      expr = true,
      noremap = previous.noremap == 1,
      silent = previous.silent == 1,
      nowait = previous.nowait == 1,
      script = previous.script == 1,
      replace_keycodes = translated,
      callback = function()
        local result = callback()
        if translated and type(result) == "string" then
          note_replay_keys(lhs, remapped, termcodes(result))
        else
          note_replay_keys(lhs, remapped, result)
        end
        return result
      end,
    })
  elseif previous.rhs and previous.rhs ~= "" then
    -- `mapset` brings the map's script id along, which `<SID>` in the
    -- expression needs; `nvim_buf_set_keymap` has none.
    local id = lhs:gsub("%W", "_")
    kept_keys[id] = lhs
    local copy = vim.tbl_extend("force", previous, {
      mode = "n",
      buffer = 1, -- the current buffer, as `previous` may be a global map
      lhs = name,
      lhsraw = termcodes(name),
      rhs = string.format(
        "v:lua.require'emeraldian'.expr_result('%s', %d, %s)",
        id,
        remapped and 1 or 0,
        previous.rhs
      ),
    })
    copy.lhsrawalt = nil
    previous.kept = pcall(vim.api.nvim_buf_call, buf, function()
      vim.fn.mapset(copy)
    end)
  end
end

--- `lhs` with no map and no link: its default, in front of what is typed ahead
--- (`i`) so the key finishes before the keys that came after it.
local function native_default(lhs)
  local count = vim.v.count > 0 and tostring(vim.v.count) or ""
  vim.api.nvim_feedkeys(count .. termcodes(lhs), "ni", false)
end

--- What `lhs` did before the bridge took it over: the user's own map if there
--- was one, the key's default otherwise.
local function fallback(lhs, previous)
  local count = vim.v.count > 0 and tostring(vim.v.count) or ""
  if previous and previous.kept then
    if not replay_allowed() then
      return
    end
    -- The kept map is found by remapping; what it returns is remapped or not
    -- as the map itself says. "i": in front of what is typed ahead.
    return vim.api.nvim_feedkeys(count .. termcodes(kept_name(lhs)), "mi", false)
  end
  if previous and previous.expr ~= 1 then
    if previous.callback then
      return previous.callback()
    elseif previous.rhs and previous.rhs ~= "" then
      if not replay_allowed() then
        return
      end
      local rhs = previous.rhs
      if previous.sid and previous.sid > 0 then
        rhs = rhs:gsub("<SID>", "<SNR>" .. previous.sid .. "_")
      end
      local keys = termcodes(rhs)
      local remapped = previous.noremap ~= 1 and previous.script ~= 1
      note_replay_keys(lhs, remapped, keys)
      return vim.api.nvim_feedkeys(count .. keys, remapped and "mi" or "ni", false)
    end
  end
  native_default(lhs)
end

--- The bridge's map for `lhs`.
local function follow_or_fallback(lhs, previous)
  if replay.armed[lhs] then
    -- The leading `lhs` of a replay's result: not remapped, so its default.
    replay.armed[lhs] = nil
    return native_default(lhs)
  end
  if not M.follow() then
    fallback(lhs, previous)
  end
end

--- `]` / `[`. A count jumps that many links. No link in the buffer: nothing
--- moves. Normal mode only, so Insert, Visual and operator-pending keep the
--- built-in bracket motions.
local function jump_link(delta)
  if not markdown_buf(vim.api.nvim_get_current_buf()) then
    return
  end
  local cursor = vim.api.nvim_win_get_cursor(0)
  local count = vim.v.count > 0 and vim.v.count or 1
  local pos = M.next_link(
    vim.api.nvim_buf_get_lines(0, 0, -1, false),
    cursor[1],
    cursor[2],
    delta * count
  )
  if pos then
    vim.api.nvim_win_set_cursor(0, pos)
  end
end

--- A vault note buffer: Markdown, or one of the standalone diagram files
--- emeraldian opens as a note (`.excalidraw`, `.mmd`, `.mermaid`).
local function note_file(name)
  local base = name:match("[^/]*$"):lower()
  return base:match("%.md$")
    or base:match("%.excalidraw$")
    or base:match("%.mmd$")
    or base:match("%.mermaid$")
end

local function attach(buf)
  if vim.b[buf].emeraldian_attached or not note_buf(buf) or not note_file(vim.api.nvim_buf_get_name(buf)) then
    return
  end
  vim.b[buf].emeraldian_attached = true
  for _, lhs in ipairs({ "<CR>", "gf", "<C-]>" }) do
    local previous = vim.api.nvim_buf_call(buf, function()
      return vim.fn.maparg(lhs, "n", false, true)
    end)
    if previous.expr == 1 then
      keep_expr_map(buf, lhs, previous)
    end
    vim.keymap.set("n", lhs, function()
      follow_or_fallback(lhs, previous)
    end, { buffer = buf, silent = true, desc = "emeraldian: follow the link under the cursor" })
  end
  -- A user's own `]` / `[` wins. Mapping these shadows the built-in `]x` / `[x`
  -- motions, so a map the user already set is kept and reported instead.
  local kept = {}
  for _, lhs in ipairs({ "]", "[" }) do
    local previous = vim.api.nvim_buf_call(buf, function()
      return vim.fn.maparg(lhs, "n", false, true)
    end)
    if previous.rhs or previous.callback then
      kept[#kept + 1] = lhs
    else
      local delta = lhs == "]" and 1 or -1
      vim.keymap.set("n", lhs, function()
        jump_link(delta)
      end, {
        buffer = buf,
        silent = true,
        nowait = true,
        desc = lhs == "]" and "emeraldian: next link" or "emeraldian: previous link",
      })
    end
  end
  if #kept > 0 then
    send("notice", "kept your " .. table.concat(kept, " ") .. " map; that key does not jump between links")
  end
  vim.keymap.set("n", "ZZ", function()
    if M.last_window() then
      M.leave(true)
    else
      vim.cmd("x")
    end
  end, { buffer = buf, silent = true, desc = "emeraldian: save and leave editing" })
end

-- Setup ----------------------------------------------------------------------

function M.setup(channel, options)
  chan = channel
  opts = options or {}
  root = (opts.root or ""):gsub("/+$", "")

  local group = vim.api.nvim_create_augroup("emeraldian", { clear = true })
  local function on(event, callback, extra)
    vim.api.nvim_create_autocmd(
      event,
      vim.tbl_extend("force", { group = group, callback = callback }, extra or {})
    )
  end

  on("BufWritePost", function(args)
    if note_buf(args.buf) then
      send("written", vim.api.nvim_buf_get_name(args.buf))
    end
  end)
  on("BufModifiedSet", function(args)
    if note_buf(args.buf) then
      send("modified", vim.api.nvim_buf_get_name(args.buf), vim.bo[args.buf].modified)
    end
  end)
  on("BufEnter", function(args)
    if note_buf(args.buf) then
      attach(args.buf)
      send("entered", vim.api.nvim_buf_get_name(args.buf))
    end
  end)
  on("FileType", function(args)
    attach(args.buf)
  end, { pattern = { "markdown", "excalidraw", "mermaid", "mmd" } })
  -- `:q`, `:wq` and `:x` on the last window are typed through the abbreviations
  -- below; this catches the other ways out (`:q!`, `:qa`, `<C-w>q`), which end
  -- nvim: emeraldian is told first so it goes back to reading.
  on("QuitPre", function()
    if M.last_window() then
      send("leave")
    end
  end)

  if opts.auto_save then
    on({ "TextChanged", "InsertLeave" }, function(args)
      if note_buf(args.buf) then
        schedule_update(args.buf)
      end
    end)
    on("BufLeave", function(args)
      stop_timer(args.buf)
      M.update(args.buf)
    end)
    on("FocusLost", function()
      for _, buf in ipairs(vim.api.nvim_list_bufs()) do
        stop_timer(buf)
        M.update(buf)
      end
    end)
    on({ "BufDelete", "BufWipeout" }, function(args)
      stop_timer(args.buf)
    end)
  end

  local actions = opts.actions or {}
  vim.api.nvim_create_user_command("Emeraldian", function(command)
    local name = command.fargs[1]
    if M.is_action(actions, name) then
      send("action", name)
    else
      vim.api.nvim_echo({ { "Emeraldian: unknown action " .. tostring(name), "ErrorMsg" } }, true, {})
    end
  end, {
    nargs = 1,
    complete = function()
      return actions
    end,
    desc = "run an emeraldian action",
  })

  -- Only when the bare word is the whole command line and the typed character
  -- is <CR>: `:q!` must stay nvim's.
  for _, word in ipairs({ "q", "quit" }) do
    vim.cmd(
      ([[cnoreabbrev <expr> %s (getcmdtype() == ':' && getcmdline() == '%s' && v:char == "\r" && luaeval('require("emeraldian").last_window()')) ? 'lua require("emeraldian").leave(false)' : '%s']]):format(
        word,
        word,
        word
      )
    )
  end
  for _, word in ipairs({ "wq", "x", "xit", "exit" }) do
    vim.cmd(
      ([[cnoreabbrev <expr> %s (getcmdtype() == ':' && getcmdline() == '%s' && v:char == "\r" && luaeval('require("emeraldian").last_window()')) ? 'lua require("emeraldian").leave(true)' : '%s']]):format(
        word,
        word,
        word
      )
    )
  end

  -- Buffers that were there before the bridge was.
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    attach(buf)
  end
end

package.loaded["emeraldian"] = M
if chan then
  M.setup(chan, opts)
  -- Injected over RPC: whatever the chunk returns is sent back, and a table of
  -- functions cannot be.
  return nil
end
return M
