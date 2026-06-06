// Minimal MCP server for testing flint's MCP integration.
// Exposes two tools: echo, add.

function send(obj) {
  const msg = JSON.stringify(obj);
  process.stdout.write(msg + '\n');
}

function handleRequest(msg) {
  const { id, method, params } = msg;

  switch (method) {
    case 'initialize':
      send({
        jsonrpc: '2.0',
        id,
        result: {
          protocolVersion: '2024-11-05',
          capabilities: { tools: {} },
          serverInfo: { name: 'test-echo-server', version: '0.1.0' },
        },
      });
      break;

    case 'notifications/initialized':
      break;

    case 'tools/list':
      send({
        jsonrpc: '2.0',
        id,
        result: {
          tools: [
            {
              name: 'echo',
              description: 'Echo back the input message',
              inputSchema: {
                type: 'object',
                properties: {
                  message: { type: 'string', description: 'Message to echo' },
                },
                required: ['message'],
              },
            },
            {
              name: 'add',
              description: 'Add two numbers',
              inputSchema: {
                type: 'object',
                properties: {
                  a: { type: 'number', description: 'First number' },
                  b: { type: 'number', description: 'Second number' },
                },
                required: ['a', 'b'],
              },
            },
          ],
        },
      });
      break;

    case 'tools/call': {
      const { name, arguments: args } = params;
      if (name === 'echo') {
        send({
          jsonrpc: '2.0',
          id,
          result: {
            content: [{ type: 'text', text: 'Echo: ' + args.message }],
            isError: false,
          },
        });
      } else if (name === 'add') {
        const sum = (args.a || 0) + (args.b || 0);
        send({
          jsonrpc: '2.0',
          id,
          result: {
            content: [{ type: 'text', text: args.a + ' + ' + args.b + ' = ' + sum }],
            isError: false,
          },
        });
      } else {
        send({
          jsonrpc: '2.0',
          id,
          result: {
            content: [{ type: 'text', text: 'Unknown tool: ' + name }],
            isError: true,
          },
        });
      }
      break;
    }

    default:
      if (id !== undefined) {
        send({
          jsonrpc: '2.0',
          id,
          error: { code: -32601, message: 'Method not found: ' + method },
        });
      }
  }
}

// Read JSON-RPC messages from stdin (one per line)
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let lines = buffer.split('\n');
  buffer = lines.pop();
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed) {
      try {
        const msg = JSON.parse(trimmed);
        handleRequest(msg);
      } catch (e) {
        // ignore parse errors
      }
    }
  }
});
