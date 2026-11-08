[日本語](command-browser.ja.md)

# Command Browser

Open **Edit > Command Browser** to run an editor action or an MCP tool from the editor.

## Run a command

1. Search by label, category or English words, such as `new file`.
2. Select a row. Editor actions and MCP tools share the list.
3. For a tool, fill in **Arguments** with a JSON object. The panel lists required fields.
4. Choose **Run Command**. A disabled command does not run or close the Browser.

Editor actions normally close the Browser after activation, including when the editor refuses the action.
Check the editor's status message. The two actions that open the Browser keep its current inputs instead.
Tools normally keep it open and show **Result**. Closing it clears its search, inputs and result.

## Document and selection changes

A command waiting to start is refused if another operation replaces the document or adopts a different save path.
Changing the path away and back still invalidates that queued command. Node actions also recheck their selected
nodes and clipboard; timeline actions recheck the selected clip, resolved playhead and values to be keyed. A stale command keeps the
Browser open. Review the current document and command before running it again.

**Save** uses the document's actual saved path, even if the toolbar contains other text. For an untitled document,
it opens the Save As dialog. **Open** and **Save As** only open their dialogs; no file is opened or saved until
you complete the dialog. **New** follows the existing unsaved-changes setting and Save/Don't Save/Cancel dialog.

## Read a result

An unknown result does not mean the command had no effect. Inspect the document before retrying. A long result
may be shortened. Starting a background export confirms that the job started; use its status to check completion.

The MCP visibility operation and its state read are documented in the [MCP API](../doc/mcp_api.md).
