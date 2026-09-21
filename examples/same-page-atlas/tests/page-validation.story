name: The page returns deterministic failures to the connected model
url: http://127.0.0.1:8098/
workflow:
1. Use an isolated document and a real Chrome tab. Call atlas_session and atlas_read through MCP before editing.
2. Before a browser render, atlas_validate must return isError true with pending status. No browser is not a passing result.
3. Open the document. Wait for measured geometry and a matching browser report. atlas_validate must pass, and the header must show the same status.
4. Move one card over another through MCP. Require isError true, card_overlap, both ids, and operation_applied true. Verify the move actually landed, without duplicating either card.
5. Repair the overlap. The page and atlas_validate must pass after the measured render reaches the host.
6. Park a real MCP await_input call. Move a card over another through the browser. The waiting call must return isError true and the affected ids without an atlas_read poll.
7. Repair the geometry and park another await_input. Trigger an uncaught browser exception. Require a tool error naming that exception without any CRDT change. The header must report failure.
8. Reload successfully. Require the runtime error to clear and validation to pass. A delayed report from before the error must never overwrite the newer result.
9. Verify a report about an older document, a report without measurements, and an expired browser observation cannot pass validation.
10. Inspect the person's current page after installing the change. Preserve saved positions, relationships, proposal state and the visible minimap. Close only the isolated test tab and server.
