const fs = require("fs/promises");
const path = require("path");

function isPathInside(rootPath, candidatePath) {
  if (!rootPath || !candidatePath) {
    return false;
  }
  const root = path.resolve(rootPath);
  const candidate = path.resolve(candidatePath);
  const relative = path.relative(root, candidate);
  return relative === "" || (!relative.startsWith(".." + path.sep) && !path.isAbsolute(relative));
}

async function canonicalPath(candidate) {
  try {
    return await fs.realpath(candidate);
  } catch {
    return path.resolve(candidate);
  }
}

async function filterBridgeCandidates(candidates, workspaceRoot, workspaceTrusted) {
  const canonicalWorkspace = workspaceRoot ? await canonicalPath(workspaceRoot) : null;
  const seen = new Set();
  const filtered = [];
  for (const candidate of candidates) {
    if (!candidate) {
      continue;
    }
    const canonicalCandidate = await canonicalPath(candidate);
    if (seen.has(canonicalCandidate)) {
      continue;
    }
    seen.add(canonicalCandidate);
    if (workspaceTrusted || !isPathInside(canonicalWorkspace, canonicalCandidate)) {
      filtered.push(candidate);
    }
  }
  return filtered;
}

function validateBridgeCapabilities(capabilities, expectedProtocolVersion) {
  if (!capabilities || capabilities.protocolVersion !== expectedProtocolVersion) {
    return {
      ok: false,
      reason: `Unsupported bridge protocol version: ${capabilities?.protocolVersion ?? "missing"}`,
    };
  }
  const commands = new Set(capabilities.commands || []);
  if (!commands.has("dataset") || !commands.has("fft")) {
    return { ok: false, reason: "Bridge does not advertise dataset and fft commands." };
  }
  return { ok: true, reason: "" };
}

function isBridgeUnavailableError(error) {
  return Boolean(error && error.code === "SCOPE_BRIDGE_UNAVAILABLE");
}

module.exports = {
  filterBridgeCandidates,
  isPathInside,
  isBridgeUnavailableError,
  validateBridgeCapabilities,
};
