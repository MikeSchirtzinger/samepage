const url = process.env.AG_UI_PI_MCP_URL;
const token = process.env.AG_UI_PI_MCP_TOKEN;
const nonce = process.env.AG_UI_PI_BRIDGE_NONCE;
const encodedCatalog = process.env.AG_UI_PI_ACTION_CATALOG;
const encodedAllowedTools = process.env.AG_UI_PI_ALLOWED_TOOLS;

// Do not leave the runtime-owned bearer token available to Pi's bash children.
delete process.env.AG_UI_PI_MCP_URL;
delete process.env.AG_UI_PI_MCP_TOKEN;
delete process.env.AG_UI_PI_BRIDGE_NONCE;
delete process.env.AG_UI_PI_ACTION_CATALOG;
delete process.env.AG_UI_PI_ALLOWED_TOOLS;

if (!url || !token || !nonce || !encodedCatalog || !encodedAllowedTools) {
	throw new Error("AG-UI Pi bridge environment is incomplete");
}

const catalog = JSON.parse(encodedCatalog);
const allowedTools = JSON.parse(encodedAllowedTools);
if (!Array.isArray(catalog) || catalog.some((tool) =>
	typeof tool?.name !== "string" ||
	typeof tool?.description !== "string" ||
	tool?.inputSchema?.type !== "object"
)) {
	throw new Error("AG-UI Pi bridge catalog is invalid");
}
if (!Array.isArray(allowedTools) || allowedTools.some((name) => typeof name !== "string")) {
	throw new Error("AG-UI Pi bridge tool allowlist is invalid");
}

let nextId = 1;
let negotiated = false;
let activated = false;

async function rpc(method, params, signal) {
	const id = nextId++;
	const headers = {
		"authorization": `Bearer ${token}`,
		"content-type": "application/json",
	};
	if (negotiated) {
		headers["mcp-protocol-version"] = "2025-06-18";
	}
	const response = await fetch(url, {
		method: "POST",
		headers,
		body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
		signal,
	});
	if (!response.ok) {
		throw new Error(`AG-UI MCP ${method} returned HTTP ${response.status}`);
	}
	const message = await response.json();
	if (message?.jsonrpc !== "2.0" || message?.id !== id) {
		throw new Error(`AG-UI MCP ${method} returned an invalid response`);
	}
	if (message.error) {
		throw new Error(`AG-UI MCP ${method} failed: ${message.error.message ?? "unknown error"}`);
	}
	return message.result;
}

async function notify(method, params) {
	const response = await fetch(url, {
		method: "POST",
		headers: {
			"authorization": `Bearer ${token}`,
			"content-type": "application/json",
			"mcp-protocol-version": "2025-06-18",
		},
		body: JSON.stringify({ jsonrpc: "2.0", method, params }),
	});
	if (response.status !== 202) {
		throw new Error(`AG-UI MCP ${method} notification returned HTTP ${response.status}`);
	}
}

async function initialize() {
	const result = await rpc("initialize", {
		protocolVersion: "2025-06-18",
		capabilities: {},
		clientInfo: { name: "ag-ui-pi-bridge", version: "0.1.0" },
	});
	if (result?.protocolVersion !== "2025-06-18") {
		throw new Error(`AG-UI MCP negotiated unsupported version ${result?.protocolVersion}`);
	}
	negotiated = true;
	await notify("notifications/initialized", {});

	const listed = await rpc("tools/list", {});
	const expected = JSON.stringify(catalog);
	const actual = JSON.stringify(listed?.tools);
	if (actual !== expected) {
		throw new Error("AG-UI MCP catalog differs from the tools registered with Pi");
	}
}

export default function agUiPiBridge(pi) {
	for (const tool of catalog) {
		pi.registerTool({
			name: tool.name,
			label: tool.name,
			description: tool.description,
			parameters: tool.inputSchema,
			executionMode: "sequential",
			async execute(_toolCallId, params, signal) {
				if (!activated) {
					throw new Error("AG-UI actions are locked until setup completes");
				}
				const result = await rpc("tools/call", {
					name: tool.name,
					arguments: params,
				}, signal);
				if (!Array.isArray(result?.content)) {
					throw new Error(`AG-UI action ${tool.name} returned invalid content`);
				}
				if (result.isError) {
					const detail = result.content
						.filter((item) => item?.type === "text")
						.map((item) => item.text)
						.join("\n");
					throw new Error(detail || `AG-UI action ${tool.name} failed`);
				}
				return {
					content: result.content,
					details: { transport: "ag-ui-mcp" },
				};
			},
		});
	}

	pi.registerCommand("ag-ui-activate", {
		description: "Activate the host-approved Pi tool allowlist after setup",
		handler: async (_args, ctx) => {
			pi.setActiveTools(allowedTools);
			activated = true;
			ctx.ui.setStatus("ag-ui-bridge", `active:${nonce}`);
		},
	});

	pi.on("session_start", async (_event, ctx) => {
		// The setup-only prime is structurally tool-free, not just prompted to
		// avoid tools. The host activates this exact list after READY settles.
		pi.setActiveTools([]);
		await initialize();
		ctx.ui.setStatus("ag-ui-bridge", `ready:${nonce}`);
	});
}
