-- SPDX-License-Identifier: MIT OR Apache-2.0
-- ERPForAll.app: a small control window for the local ERPForAll server on a Mac.
-- build-mac-app.sh compiles this file with osacompile and replaces the placeholder below with the absolute
-- path of erpforall-ctl in your checkout. Every action runs erpforall-ctl; this file only shows dialogs.

property ctlPath : "@@ERPFORALL_CTL@@"
property appTitle : "ERPForAll"

on run
	if not my ctlExists() then
		display dialog "The ERPForAll checkout this app was built for is gone:" & return & ctlPath & return & return & "Run scripts/macos/build-mac-app.sh again from the checkout's new place." with title appTitle buttons {"OK"} default button "OK" with icon stop
		return
	end if
	repeat
		set st to my ctlOrReport("status")
		if st is missing value then return
		if st is "not-installed" then
			set choice to my askChoice("ERPForAll is not installed on this Mac yet." & return & return & "The first install builds it from source and creates a demo database. It can take 10 to 30 minutes, so it runs in Terminal where you can follow it.", {"Quit", "Install in Terminal"}, "Install in Terminal")
			if choice is "Install in Terminal" then
				my runInTerminal("install")
				display dialog "The install is running in Terminal. Open this app again when Terminal says Done." with title appTitle buttons {"OK"} default button "OK" with icon note
			end if
			return
		else if st is "stopped" then
			set choice to my askChoice("ERPForAll is stopped.", {"Quit", "More...", "Start"}, "Start")
			if choice is "Quit" then return
			if choice is "Start" then
				if my startServer() then return
			else
				if my moreMenu(st) then return
			end if
		else if st is "running" then
			set choice to my askChoice("ERPForAll is running at" & return & my ctlOrReport("url"), {"Stop", "More...", "Open"}, "Open")
			if choice is "Open" then
				my ctlOrReport("open")
				return
			else if choice is "Stop" then
				my runAction("stop", "Stopping ERPForAll...")
			else
				if my moreMenu(st) then return
			end if
		else if st is "starting" then
			set choice to my askChoice("ERPForAll is starting. Wait a moment, then refresh.", {"Quit", "More...", "Refresh"}, "Refresh")
			if choice is "Quit" then return
			if choice is "More..." then
				if my moreMenu(st) then return
			end if
		else if st is "port-busy" then
			set choice to my askChoice("ERPForAll is stopped, but its port is used by another program (a local Odoo, or an ERPForAll started by hand?). Stop that program, then refresh.", {"Quit", "More...", "Refresh"}, "Refresh")
			if choice is "Quit" then return
			if choice is "More..." then
				if my moreMenu(st) then return
			end if
		else
			display dialog "erpforall-ctl answered an unknown state: " & st with title appTitle buttons {"OK"} default button "OK" with icon stop
			return
		end if
	end repeat
end run

-- Starts the server and opens the browser. Returns true when the app should quit.
on startServer()
	if my runAction("start", "Starting ERPForAll. This can take a minute...") then
		my ctlOrReport("open")
		return true
	end if
	return false
end startServer

-- The longer list of actions. Returns true when the app should quit.
on moreMenu(st)
	set items_ to {}
	if st is "running" then set end of items_ to "Open in browser"
	if st is "stopped" then set end of items_ to "Start"
	if st is "running" or st is "starting" then
		set end of items_ to "Stop server and database"
		set end of items_ to "Stop server only (keep the database container)"
	end if
	set end of items_ to "Copy admin password"
	set end of items_ to "Show server log"
	set end of items_ to "Rebuild after git pull (in Terminal)"
	set end of items_ to "Details"
	if st is "running" then
		set end of items_ to "Quit (ERPForAll keeps running)"
	else
		set end of items_ to "Quit"
	end if
	set picked to choose from list items_ with title appTitle with prompt "ERPForAll: " & st OK button name "Run" cancel button name "Back"
	if picked is false then return false
	set chosen to item 1 of picked
	if chosen is "Open in browser" then
		my ctlOrReport("open")
		return true
	else if chosen is "Start" then
		return my startServer()
	else if chosen is "Stop server and database" then
		my runAction("stop", "Stopping ERPForAll...")
	else if chosen starts with "Stop server only" then
		my runAction("stop --keep-db", "Stopping the ERPForAll server...")
	else if chosen is "Copy admin password" then
		set pwFile to my ctlOrReport("password-file")
		if pwFile is not missing value then
			try
				do shell script "tr -d '\\n' < " & quoted form of pwFile & " | pbcopy"
				display notification "Login: admin. The password is on the clipboard." with title appTitle
			on error errMsg
				my report(errMsg)
			end try
		end if
	else if chosen is "Show server log" then
		set logFile to my ctlOrReport("log-path")
		if logFile is not missing value then
			try
				do shell script "test -f " & quoted form of logFile & " && open -a Console " & quoted form of logFile
			on error
				my report("There is no server log yet: " & logFile)
			end try
		end if
	else if chosen starts with "Rebuild" then
		my runInTerminal("rebuild")
		return true
	else if chosen is "Details" then
		set infoText to my ctlOrReport("info")
		if infoText is not missing value then display dialog infoText with title appTitle buttons {"OK"} default button "OK"
	else if chosen starts with "Quit" then
		return true
	end if
	return false
end moreMenu

on askChoice(msg, btns, defaultBtn)
	return button returned of (display dialog msg with title appTitle buttons btns default button defaultBtn with icon note)
end askChoice

on ctlExists()
	try
		do shell script "test -x " & quoted form of ctlPath
		return true
	on error
		return false
	end try
end ctlExists

on ctl(args)
	-- Starting waits for Docker (up to 2 minutes) and for the server to be ready (up to 3 minutes).
	with timeout of 900 seconds
		return do shell script quoted form of ctlPath & " " & args
	end timeout
end ctl

-- Runs erpforall-ctl and returns its output, or shows the error and returns missing value.
on ctlOrReport(args)
	try
		return my ctl(args)
	on error errMsg
		my report(errMsg)
		return missing value
	end try
end ctlOrReport

-- Runs a longer action with a notification first. Returns true when it succeeded.
on runAction(args, busyMsg)
	display notification busyMsg with title appTitle
	try
		my ctl(args)
		return true
	on error errMsg
		my report(errMsg)
		return false
	end try
end runAction

on report(errMsg)
	if (length of errMsg) > 1500 then set errMsg to "..." & (text -1500 thru -1 of errMsg)
	display dialog errMsg with title appTitle buttons {"OK"} default button "OK" with icon caution
end report

on runInTerminal(args)
	tell application "Terminal"
		activate
		do script quoted form of ctlPath & " " & args
	end tell
end runInTerminal
