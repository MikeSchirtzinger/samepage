# Demo runbook

## Main demo: one shared room

Use two terminals from the repository root. Keep the room visible at
<http://127.0.0.1:8100> while the second terminal drives its MCP door.

### Terminal 1: start the room

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

The room has no frontend build step. Wait for `listening on
http://127.0.0.1:8100`, then open that address in Chrome.

### Terminal 2: attach a named agent

Initialize without a session id. The host returns the session id in a response
header and also returns the host-minted participant in the JSON body.

```bash
export AGUI_MCP_TOKEN=local-room-token
MCP_HEADERS=$(mktemp /tmp/samepage-mcp-headers.XXXXXX)

curl --silent --show-error --fail-with-body --dump-header "$MCP_HEADERS" \
  --request POST http://127.0.0.1:8100/mcp \
  --header "Authorization: Bearer $AGUI_MCP_TOKEN" \
  --header 'Content-Type: application/json' \
  --header 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Demo Agent","version":"1.0"}}}' | jq .

MCP_SESSION_ID=$(awk 'tolower($1) == "mcp-session-id:" {gsub("\\r", "", $2); print $2}' "$MCP_HEADERS")
test -n "$MCP_SESSION_ID"
printf 'MCP_SESSION_ID=%s\n' "$MCP_SESSION_ID"
```

Send `MCP-Protocol-Version: 2025-06-18` and the same `Mcp-Session-Id` on every
request after initialization. Dropping the session header keeps agent
permissions but downgrades writes to the generic `agent` byline.

```bash
curl --silent --show-error --fail-with-body --output /dev/null \
  --write-out 'initialized_http=%{http_code}\n' \
  --request POST http://127.0.0.1:8100/mcp \
  --header "Authorization: Bearer $AGUI_MCP_TOKEN" \
  --header 'MCP-Protocol-Version: 2025-06-18' \
  --header "Mcp-Session-Id: $MCP_SESSION_ID" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized"}'

mcp() {
  curl --silent --show-error --fail-with-body \
    --request POST http://127.0.0.1:8100/mcp \
    --header "Authorization: Bearer $AGUI_MCP_TOKEN" \
    --header 'MCP-Protocol-Version: 2025-06-18' \
    --header "Mcp-Session-Id: $MCP_SESSION_ID" \
    --header 'Content-Type: application/json' \
    --header 'Accept: application/json, text/event-stream' \
    --data "$1"
}

mcp '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | jq -r '.result.tools[].name'
```

The tool list must include `put_pane`, `remove_pane`, `arrange_room`,
`configure_room`, `await_room`, and `read_room`. It must not include the
browser-only annotation action.

### Live loop

Read before writing and take the current revision from the returned room text.

```bash
MCP_READ=$(mcp '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_room","arguments":{}}}')
printf '%s\n' "$MCP_READ" | jq -r '.result.content[0].text'
ROOM_REVISION=$(printf '%s\n' "$MCP_READ" | jq -r '.result.content[0].text | capture("THE ROOM . revision (?<n>[0-9]+)").n')

MCP_PUT=$(jq -cn --argjson revision "$ROOM_REVISION" '
  {jsonrpc:"2.0",id:5,method:"tools/call",params:{name:"put_pane",arguments:{
    expected_revision:$revision,
    id:"demo-loop",
    title:"Named MCP session",
    view:{kind:"stack",children:[
      {kind:"badge",text:"MCP session attached",tone:"good"},
      {kind:"text",text:"The host stamped this pane as Demo Agent because every call kept the session header."}
    ]},
    place:"below: start",
    size:"wide"
  }}}')
mcp "$MCP_PUT" | jq .
```

Chrome must show a new `Named MCP session` pane with the `Demo Agent` byline.
This proves the session header reached host attribution rather than only the
JSON response.

In Chrome, click `?` on that pane. Click `note`, enter `Show the mark surviving
the rewrite.`, and press Return. Then read the room again:

```bash
MCP_READ_AFTER=$(mcp '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"read_room","arguments":{}}}')
printf '%s\n' "$MCP_READ_AFTER" | jq -r '.result.content[0].text' \
  | sed -n '1p;/CHANGED SINCE YOUR LAST READ/,/HOW TO CHANGE THE ROOM/p'
ROOM_REVISION=$(printf '%s\n' "$MCP_READ_AFTER" | jq -r '.result.content[0].text | capture("THE ROOM . revision (?<n>[0-9]+)").n')

MCP_REWRITE=$(jq -cn --argjson revision "$ROOM_REVISION" '
  {jsonrpc:"2.0",id:8,method:"tools/call",params:{name:"put_pane",arguments:{
    expected_revision:$revision,
    id:"demo-loop",
    title:"Named MCP session",
    view:{kind:"stack",children:[
      {kind:"badge",text:"Question read",tone:"good"},
      {kind:"text",text:"The question and note arrived through read_room. Reusing the pane id leaves the mark attached."}
    ]}
  }}}')
mcp "$MCP_REWRITE" | jq .
```

The delta must name the human `?` and note. After the rewrite, Chrome must keep
the `?`, note, pane position, and `Demo Agent` byline. That is the main loop:
the agent changes the shared artifact, the person disagrees in place, the agent
reads that change, and the same artifact is revised without erasing the
person's contribution.

Press Control-C in terminal 1 when the demo is finished.
