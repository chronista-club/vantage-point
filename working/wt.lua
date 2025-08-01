#!/usr/bin/env lua
-- Worktree management helper with cd support
-- Usage: eval $(lua working/wt.lua cd <name>)

local function execute(cmd)
    local handle = io.popen(cmd)
    local result = handle:read("*a")
    local success = handle:close()
    return result, success
end

local function dir_exists(path)
    local ok, err, code = os.rename(path, path)
    if not ok then
        if code == 13 then
            return true
        end
    end
    return ok
end

local script_dir = arg[0]:match("(.*/)")
local working_dir = script_dir or "working/"
local command = arg[1] or "help"

if command == "cd" or command == "switch" then
    local name = arg[2]
    
    if not name then
        io.stderr:write("❌ Usage: lua " .. arg[0] .. " cd <name>\n")
        io.stderr:write("   Available worktrees:\n")
        local handle = io.popen("ls -1 " .. working_dir .. " 2>/dev/null")
        if handle then
            for file in handle:lines() do
                if file ~= "README.md" and file ~= ".gitkeep" and file ~= "wt.lua" then
                    io.stderr:write("     - " .. file .. "\n")
                end
            end
            handle:close()
        else
            io.stderr:write("     (none)\n")
        end
        os.exit(1)
    end
    
    local worktree_path = working_dir .. name
    
    if not dir_exists(worktree_path) then
        io.stderr:write("❌ Worktree '" .. name .. "' not found\n")
        os.exit(1)
    end
    
    -- Output the cd command for shell evaluation
    print("cd '" .. worktree_path .. "'")
    
else
    io.stderr:write("This script is meant to be used with eval:\n")
    io.stderr:write("  eval $(lua " .. arg[0] .. " cd <worktree-name>)\n")
end