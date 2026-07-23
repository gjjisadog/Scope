const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("fs/promises");
const os = require("os");
const path = require("path");
const {
  filterBridgeCandidates,
  isBridgeUnavailableError,
  validateBridgeCapabilities,
} = require("./bridge-policy");

test("untrusted workspace rejects candidates inside the workspace", async () => {
  const candidates = await filterBridgeCandidates(
    [
      "/workspace/target/debug/scope_analyzer",
      "/opt/scope/scope_analyzer",
    ],
    "/workspace",
    false
  );

  assert.deepEqual(candidates, ["/opt/scope/scope_analyzer"]);
});

test("trusted workspace permits the explicitly discovered workspace bridge", async () => {
  const candidates = await filterBridgeCandidates(
    ["/workspace/target/debug/scope_analyzer"],
    "/workspace",
    true
  );

  assert.deepEqual(candidates, ["/workspace/target/debug/scope_analyzer"]);
});

test("untrusted workspace rejects an external symlink into the workspace", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "scope-bridge-policy-"));
  const workspace = path.join(root, "workspace");
  const outside = path.join(root, "outside");
  const target = path.join(workspace, "scope_analyzer");
  const bridgeLink = path.join(outside, "scope_analyzer");
  try {
    await fs.mkdir(workspace);
    await fs.mkdir(outside);
    await fs.writeFile(target, "bridge");
    await fs.symlink(target, bridgeLink);

    const candidates = await filterBridgeCandidates([bridgeLink], workspace, false);

    assert.deepEqual(candidates, []);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("bridge capabilities require the negotiated protocol and advertised commands", () => {
  assert.deepEqual(
    validateBridgeCapabilities(
      { protocolVersion: 1, commands: ["dataset", "fft"] },
      1
    ),
    { ok: true, reason: "" }
  );
  assert.equal(
    validateBridgeCapabilities({ protocolVersion: 2, commands: ["dataset", "fft"] }, 1).ok,
    false
  );
  assert.equal(
    validateBridgeCapabilities({ protocolVersion: 1, commands: ["dataset"] }, 1).ok,
    false
  );
});

test("only an explicitly unavailable bridge may enter compatibility mode", () => {
  assert.equal(isBridgeUnavailableError({ code: "SCOPE_BRIDGE_UNAVAILABLE" }), true);
  assert.equal(isBridgeUnavailableError({ code: "SCOPE_BRIDGE_VERSION_MISMATCH" }), false);
  assert.equal(isBridgeUnavailableError(new Error("bridge execution failed")), false);
});
