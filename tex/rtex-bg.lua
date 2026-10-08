-- rtex-bg.lua: standby background pass. The driver has loaded the document's preamble; block
-- here until the host writes "GO" on stdin (the body snapshot is complete), then return so
-- \input{main.tex} typesets the body. Anything else (EOF, "QUIT") ends the process quietly.
local line = io.read("l")
if line ~= "GO" then os.exit(0) end
