#!/usr/bin/env node
// The reference plugin: the whole host protocol in plain Node, no dependencies.
//
// It exists to prove the language claim in docs/09-platform-tier.md §2.2 — that
// a plugin author writes their language against PROTOCOL.md and the host does
// not change. So it implements every method, not just the ones the tests call:
// a host method with no example here is a method nobody can check the document
// against.
//
// Rules this file follows, and why:
//   - stdout carries protocol frames and nothing else. A `console.log` would be
//     a malformed line to the host, so diagnostics go to stderr, which the host
//     captures and logs.
//   - every request is answered with the same id, exactly. The host refuses an
//     id that differs by one, deliberately: a plugin that multiplexed replies
//     would otherwise hand one call's answer to another.
//   - a tool that fails is a *result* with isError: true. A JSON-RPC error means
//     the plugin could not attempt the call at all — a different fact, and the
//     host reports it differently.
'use strict';

const fs = require('fs');
const PROTOCOL_VERSION = 1;
const BIG_BYTES = 1 << 20; // `big` returns exactly 1 MiB.
// A 16-byte unit divides 1 MiB exactly, so the expected body is unambiguous.
const BIG_UNIT = '0123456789abcdef';
const BIG_BODY = BIG_UNIT.repeat(BIG_BYTES / BIG_UNIT.length);

const TOOLS = [
  {
    name: 'echo',
    description: 'Return the text it was given, unchanged.',
    schema: {
      type: 'object',
      properties: { text: { type: 'string', description: 'Any string; it comes back byte for byte.' } },
      required: ['text'],
      additionalProperties: false,
    },
  },
  {
    name: 'add',
    description: 'Add two numbers and return the sum as text.',
    schema: {
      type: 'object',
      properties: { a: { type: 'number' }, b: { type: 'number' } },
      required: ['a', 'b'],
      additionalProperties: false,
    },
  },
  {
    name: 'fail',
    description: 'Always fails, on purpose. Proves a tool-level error is data.',
    schema: { type: 'object', properties: {}, additionalProperties: false },
  },
  {
    name: 'big',
    description: 'Return 1 MiB of text, to prove a large body arrives whole.',
    schema: { type: 'object', properties: {}, additionalProperties: false },
  },
];

const PROMPTS = [
  {
    name: 'greeting',
    description: 'One line telling the model it is talking to the reference plugin.',
  },
];

// The phases this plugin claims. The host calls nothing a plugin did not claim,
// so this list is both a promise and the only routing the plugin needs.
const HOOKS = ['before_turn'];

function write(message) {
  process.stdout.write(JSON.stringify(message) + '\n');
}

function reply(id, result) {
  write({ jsonrpc: '2.0', id: id, result: result });
}

function failRequest(id, code, message) {
  write({ jsonrpc: '2.0', id: id, error: { code: code, message: message } });
}

function toolOutput(output, isError) {
  return { output: output, isError: isError === true };
}

// -32602 invalid params, -32000 refused, -32601 method not found: the JSON-RPC
// numbers PROTOCOL.md documents. Using the documented numbers matters because
// the host prints them and somebody will grep for them.
const INVALID_PARAMS = -32602;
const REFUSED = -32000;
const METHOD_NOT_FOUND = -32601;
const PARSE_ERROR = -32700;

function handleInitialize(id, params) {
  if (!params || typeof params !== 'object') {
    return failRequest(id, INVALID_PARAMS, 'initialize needs a params object');
  }
  if (params.protocolVersion !== PROTOCOL_VERSION) {
    return failRequest(
      id,
      REFUSED,
      'this plugin speaks protocol version ' + PROTOCOL_VERSION +
        ', the host asked for ' + String(params.protocolVersion)
    );
  }
  reply(id, {
    protocolVersion: PROTOCOL_VERSION,
    name: 'reference',
    version: '1.0.0',
    capabilities: { tools: true, prompts: true, hooks: HOOKS },
  });
}

function callTool(name, args) {
  const args_ = args && typeof args === 'object' ? args : {};
  switch (name) {
    case 'echo': {
      if (typeof args_.text !== 'string') {
        return toolOutput('echo needs arguments.text to be a string', true);
      }
      return toolOutput(args_.text, false);
    }
    case 'add': {
      const a = args_.a;
      const b = args_.b;
      if (typeof a !== 'number' || typeof b !== 'number' || !isFinite(a) || !isFinite(b)) {
        return toolOutput('add needs arguments.a and arguments.b to be finite numbers', true);
      }
      return toolOutput(String(a + b), false);
    }
    case 'fail':
      return toolOutput('the fail tool always fails: this message comes from the plugin', true);
    case 'big':
      return toolOutput(BIG_BODY, false);
    default:
      return null;
  }
}

function handleToolsCall(id, params) {
  if (!params || typeof params.name !== 'string' || params.name === '') {
    return failRequest(id, INVALID_PARAMS, 'tools/call needs a tool name');
  }
  const result = callTool(params.name, params.arguments);
  if (result === null) {
    // An unknown tool is a caller bug, not a tool failure: -32602 says "ask for
    // something I advertised", which is what the host should print.
    return failRequest(id, INVALID_PARAMS, 'no tool named ' + JSON.stringify(params.name));
  }
  reply(id, result);
}

function handlePromptsGet(id, params) {
  if (!params || typeof params.name !== 'string') {
    return failRequest(id, INVALID_PARAMS, 'prompts/get needs a prompt name');
  }
  if (params.name === 'greeting') {
    return reply(id, { text: 'You are talking to the forge reference plugin. Answer in one line.' });
  }
  return failRequest(id, INVALID_PARAMS, 'no prompt named ' + JSON.stringify(params.name));
}

function handleHook(id, params) {
  if (!params || typeof params.phase !== 'string') {
    return failRequest(id, INVALID_PARAMS, 'hooks/run needs a phase');
  }
  if (HOOKS.indexOf(params.phase) < 0) {
    return failRequest(id, REFUSED, 'this plugin does not run the phase ' + JSON.stringify(params.phase));
  }
  const payload = params.payload;
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) {
    return failRequest(id, INVALID_PARAMS, 'hooks/run needs a payload object');
  }
  if (params.phase === 'before_turn') {
    // The one hook: append a newline to the turn's text, so a test can tell the
    // payload came back modified rather than merely returned.
    if (typeof payload.text !== 'string') {
      return failRequest(id, INVALID_PARAMS, 'before_turn needs payload.text to be a string');
    }
    return reply(id, { payload: { text: payload.text + '\n' } });
  }
  // A claimed phase with no handler must still answer, or the host would time
  // out waiting: silence is never a valid response.
  return reply(id, { payload: payload });
}

function handleMessage(message) {
  const hasId = Object.prototype.hasOwnProperty.call(message, 'id') && message.id !== null;
  const method = message.method;

  if (typeof method === 'string') {
    switch (method) {
      case 'initialize':
        return handleInitialize(message.id, message.params);
      case 'tools/list':
        return reply(message.id, { tools: TOOLS });
      case 'tools/call':
        return handleToolsCall(message.id, message.params);
      case 'prompts/list':
        return reply(message.id, { prompts: PROMPTS });
      case 'prompts/get':
        return handlePromptsGet(message.id, message.params);
      case 'hooks/run':
        return handleHook(message.id, message.params);
      case 'shutdown':
        // Answer first, then exit in the write callback. `process.exit()`
        // immediately after a write to a pipe can truncate it, and a plugin that
        // vanished mid-reply would look like a crash in the host's log even
        // though it shut down exactly as asked.
        process.stdout.write(
          JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} }) + '\n',
          function () {
            process.exit(0);
          }
        );
        return;
      case 'initialized':
        // The host's acknowledgement of the handshake; no reply by definition.
        return;
      default:
        if (hasId) {
          return failRequest(message.id, METHOD_NOT_FOUND, 'no method named ' + JSON.stringify(method));
        }
        // An unknown notification is ignorable by the JSON-RPC contract.
        return;
    }
  }
  if (hasId) {
    return failRequest(message.id, METHOD_NOT_FOUND, 'the request has no method');
  }
  // Neither a request nor a notification: nothing to answer.
}

// Newline-delimited framing. The buffer is a string because the host writes one
// frame per line and never splits a character across writes; a plugin that
// needed byte-exact binary would say so in its own protocol.
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', function (chunk) {
  buffer += chunk;
  let index = buffer.indexOf('\n');
  while (index >= 0) {
    const line = buffer.slice(0, index).trim();
    buffer = buffer.slice(index + 1);
    index = buffer.indexOf('\n');
    if (line === '') continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      // -32700 because that is what it is. There is no id to echo when the line
      // is not JSON, so the id is null, which JSON-RPC allows for this case.
      failRequest(null, PARSE_ERROR, 'the line is not JSON: ' + error.message);
      continue;
    }
    try {
      handleMessage(message);
    } catch (error) {
      // A thrown handler is a bug in this file, not in the host. Report it on
      // stderr so the host's error message carries the diagnosis, and answer so
      // the caller does not sit until its deadline.
      process.stderr.write('reference plugin: handler threw: ' + (error && error.stack) + '\n');
      if (message && Object.prototype.hasOwnProperty.call(message, 'id')) {
        failRequest(message.id, -32603, 'the plugin failed inside ' + String(message.method));
      }
    }
  }
});

// EOF on stdin is the host saying "stop": exit cleanly rather than linger.
process.stdin.on('end', function () {
  process.exit(0);
});

void fs;
