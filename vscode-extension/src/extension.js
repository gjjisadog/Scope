const fs = require("fs/promises");
const path = require("path");
const vscode = require("vscode");

const MAX_STANDARD_ROWS = 250000;
const MAX_CLOUD_RECORDS = 125000;
const MAX_DAT_SAMPLES = 250000;
const MAX_DAT_CHANNELS = 128;
const DAT_HEADER_FIXED_WORDS = 4;
const DAT_HEADER_WORD_BYTES = 4;
const CLOUD_CHANNEL_NAMES = [
  "stVbus_0.iVal",
  "stVbusHalf_0.iVal",
  "stMainIntVal_0.iVbusRefGPURunMwIr",
  "stVg_0.iA",
  "stVg_0.iB",
  "stVg_0.iC",
  "stIg_0.iA",
  "stIg_0.iB",
  "stIg_0.iC",
  "stVinv_0.iA",
  "stVinv_0.iB",
  "stVinv_0.iC",
  "stVboostA_0.iVal",
  "stIboostA_0.iVal",
  "stVboostB_0.iVal",
  "stIboostB_0.iVal",
  "stVboostC_0.iVal",
  "stIboostC_0.iVal",
  "stVboostD_0.iVal",
  "stIboostD_0.iVal",
  "stVbatteryOut_A.iVal",
  "stIBuckBoost_A.iVal",
  "stVbatteryOut_B.iVal",
  "stIBuckBoost_B.iVal",
  "stPIIboostA_0.iRef",
  "stPIIboostB_0.iRef",
  "stPIIgD_A.iRef",
  "stIg_0.iD_A",
  "stPIIBuckboost_A.iRef",
  "stPIIBuckboost_B.iRef",
  "LogicStsWord1.GPUOnOffSt",
  "LogicStsWord1.Fault",
  "LogicStsWord2.GPUSoftOK",
  "LogicStsWord2.WindSolarOk",
  "LogicStsWord2.OkByDrm",
  "unVbusOvPwmOffFlag.VbusOvBoostOffFlag_0",
  "VbusOvBoostAndGPUOffFlag_0",
  "VbusOvBoostAndGPUOffStandbyFlag_0",
  "LogicStsWord2.StandBySynochOk",
  "LogicStsWord2.BatteryAReady",
  "LogicStsWord2.BatteryBReady",
  "stRelayState.RelayU1V1W1",
  "stRelayState.RelayU2V2W2",
  "stRelayState.RelayGridN",
  "stRelayState.RelayInvN",
  "stRelayState.RelayBackupPE",
  "stRelayState.RelayBatteryA",
  "stRelayState.RelayBackup",
  "stRelayState.RelayGen",
  "stRelayState.RelayAcSoftStart",
  "stRelayState.RelayBatteryB",
  "unOcpFaultState.IgOCP",
  "unOcpFaultState.IboostOCP",
  "unOcpFaultState.IbuckboostOCP",
  "unOcpFaultState.ImidbusOCP",
  "unFaultFlag.GridFault",
  "unFaultFlag.BatteryFault",
  "unFaultFlag.LoadFault",
  "unFaultFlag.DeviceFault",
  "unFaultFlag.GenFault",
];

function activate(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("scopeAnalyzer.openFile", async (resource) => {
      const fileUri = resource || (await pickWaveformFile());
      if (fileUri) {
        await openAnalyzer(context, fileUri);
      }
    }),
    vscode.commands.registerCommand("scopeAnalyzer.openActiveFile", async () => {
      const active = vscode.window.activeTextEditor?.document?.uri;
      if (!active || active.scheme !== "file" || !isSupportedWaveformPath(active.fsPath)) {
        vscode.window.showWarningMessage("Open a CSV or DAT file first, then run this command.");
        return;
      }
      await openAnalyzer(context, active);
    })
  );
}

function deactivate() {}

function isSupportedWaveformPath(filePath) {
  const extension = path.extname(filePath).toLowerCase();
  return extension === ".csv" || extension === ".dat";
}

async function pickWaveformFile() {
  const [fileUri] =
    (await vscode.window.showOpenDialog({
      canSelectFiles: true,
      canSelectFolders: false,
      canSelectMany: false,
      filters: {
        "Waveform data files": ["csv", "dat"],
      },
      title: "Open waveform data",
    })) || [];
  return fileUri;
}

async function openAnalyzer(context, fileUri) {
  const panel = vscode.window.createWebviewPanel(
    "scopeAnalyzer",
    `Scope: ${path.basename(fileUri.fsPath)}`,
    vscode.ViewColumn.Beside,
    {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, "media")],
    }
  );

  panel.webview.html = getWebviewHtml(context, panel.webview);
  panel.webview.onDidReceiveMessage(
    async (message) => {
      if (message.type === "ready") {
        await loadAndPostDataset(panel, fileUri, message.options || {});
      }
      if (message.type === "reload") {
        await loadAndPostDataset(panel, fileUri, message.options || {});
      }
      if (message.type === "export") {
        await exportWaveformFile(fileUri, message.format, message.options || {});
      }
    },
    undefined,
    context.subscriptions
  );
}

async function loadAndPostDataset(panel, fileUri, options) {
  try {
    panel.webview.postMessage({ type: "loading", path: fileUri.fsPath });
    const dataset = await parseWaveformFile(fileUri.fsPath, options);
    panel.webview.postMessage({ type: "dataset", dataset });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    panel.webview.postMessage({ type: "error", message });
    vscode.window.showErrorMessage(`Scope Analyzer failed to open waveform file: ${message}`);
  }
}

async function exportWaveformFile(fileUri, format, options) {
  const dataset = await parseWaveformFile(fileUri.fsPath, options);
  const info = exportFormatInfo(format);
  const target = await vscode.window.showSaveDialog({
    defaultUri: vscode.Uri.file(path.join(path.dirname(fileUri.fsPath), exportFileName(fileUri.fsPath, info))),
    filters: {
      [info.filter]: [info.extension],
    },
    title: `Export ${info.label}`,
  });
  if (!target) {
    return;
  }
  const text = serializeDataset(dataset, info.kind);
  await vscode.workspace.fs.writeFile(target, Buffer.from(text, "utf8"));
  vscode.window.showInformationMessage(`Exported ${info.label}: ${target.fsPath}`);
}

function exportFormatInfo(format) {
  switch (format) {
    case "data-csv":
      return { kind: "data-csv", label: "DATA CSV", filter: "DATA CSV file", extension: "csv", suffix: "_data" };
    case "tsv":
      return { kind: "tsv", label: "TSV", filter: "TSV file", extension: "tsv", suffix: "" };
    case "json":
      return { kind: "json", label: "JSON", filter: "JSON file", extension: "json", suffix: "" };
    case "standard-csv":
    default:
      return { kind: "standard-csv", label: "Standard CSV", filter: "CSV file", extension: "csv", suffix: "" };
  }
}

function exportFileName(filePath, info) {
  const parsed = path.parse(filePath);
  return `${parsed.name}${info.suffix}.${info.extension}`;
}

function serializeDataset(dataset, format) {
  if (format === "json") {
    return JSON.stringify(
      {
        source_name: dataset.name,
        source_path: dataset.path,
        format: dataset.format,
        sample_rate_hz: dataset.sampleRateHz,
        sample_count: dataset.sampleCount,
        channels: dataset.channelSummaries.map((channel) => ({
          index: channel.index,
          name: channel.name,
        })),
        samples: dataset.times.map((time, rowIndex) => ({
          time,
          values: dataset.channels.map((values) => {
            const value = values[rowIndex];
            return Number.isFinite(value) ? value : null;
          }),
        })),
      },
      null,
      2
    );
  }

  if (format === "data-csv") {
    const dt = dataset.sampleRateHz > 0 ? 1 / dataset.sampleRateHz : 0;
    const lines = [
      csvRecord(["file_path", dataset.path], ","),
      csvRecord(["dt", formatNumberRaw(dt)], ","),
      csvRecord(["Number_of_Point", String(dataset.sampleCount)], ","),
      csvRecord(["END"], ","),
      csvRecord(dataset.channelSummaries.map((channel) => channel.name), ","),
    ];
    for (let rowIndex = 0; rowIndex < dataset.times.length; rowIndex += 1) {
      lines.push(
        csvRecord(
          dataset.channels.map((values) => valueField(values[rowIndex])),
          ","
        )
      );
    }
    return `${lines.join("\n")}\n`;
  }

  const delimiter = format === "tsv" ? "\t" : ",";
  const lines = [
    csvRecord(["time", ...dataset.channelSummaries.map((channel) => channel.name)], delimiter),
  ];
  for (let rowIndex = 0; rowIndex < dataset.times.length; rowIndex += 1) {
    lines.push(
      csvRecord(
        [
          formatNumberRaw(dataset.times[rowIndex]),
          ...dataset.channels.map((values) => valueField(values[rowIndex])),
        ],
        delimiter
      )
    );
  }
  return `${lines.join("\n")}\n`;
}

function valueField(value) {
  return Number.isFinite(value) ? String(value) : "";
}

function formatNumberRaw(value) {
  return Number.isFinite(value) ? String(value) : "";
}

function csvRecord(fields, delimiter) {
  return fields.map((field) => csvField(field, delimiter)).join(delimiter);
}

function csvField(value, delimiter) {
  const text = value == null ? "" : String(value);
  if (text.includes('"') || text.includes("\n") || text.includes("\r") || text.includes(delimiter)) {
    return `"${text.replace(/"/g, '""')}"`;
  }
  return text;
}

async function parseWaveformFile(filePath, options) {
  const extension = path.extname(filePath).toLowerCase();
  if (extension === ".dat") {
    const buffer = await fs.readFile(filePath);
    return parseDatFile(filePath, buffer);
  }
  if (extension !== ".csv") {
    throw new Error(`Unsupported waveform file extension: ${extension || "(none)"}`);
  }

  const text = await fs.readFile(filePath, "utf8");
  const lines = text.replace(/^\uFEFF/, "").split(/\r?\n/).filter((line) => line.trim().length > 0);
  if (lines.length < 2) {
    throw new Error("CSV file is empty or has no data rows.");
  }

  const headers = parseCsvLine(lines[0]).map((header) => header.trim().replace(/^\uFEFF/, ""));
  if ((headers[0] || "").toLowerCase() === "file_path") {
    return parseMetadataCsv(filePath, lines);
  }
  const contentColumn = headers.findIndex((header) => header.toLowerCase() === "content");
  if (contentColumn >= 0) {
    return parseCloudCsv(filePath, headers, lines.slice(1), contentColumn, options);
  }
  return parseStandardCsv(filePath, headers, lines.slice(1));
}

function parseMetadataCsv(filePath, lines) {
  let sampleInterval = null;
  let headerRow = -1;

  for (let index = 1; index < lines.length; index += 1) {
    const fields = parseCsvLine(lines[index]);
    const key = (fields.find((field) => field.trim()) || "").trim().toLowerCase();
    if (key === "dt") {
      sampleInterval = fields.slice(1).map(Number).find((value) => Number.isFinite(value) && value > 0) || null;
    }
    if (fields.some((field) => field.trim().toLowerCase().includes("end"))) {
      headerRow = index + 1;
      break;
    }
  }

  if (!sampleInterval || headerRow < 0 || headerRow >= lines.length) {
    throw new Error("Metadata CSV requires file_path/dt metadata, an END marker, and a channel header row.");
  }

  const channelNames = parseCsvLine(lines[headerRow])
    .slice(0, 128)
    .map((name, index) => name || `CH${index + 1}`);
  if (!channelNames.length) {
    throw new Error("Metadata CSV channel header row is empty.");
  }

  const channels = channelNames.map(() => []);
  const times = [];
  let skipped = 0;
  let truncated = false;

  for (let index = headerRow + 1; index < lines.length; index += 1) {
    if (times.length >= MAX_STANDARD_ROWS) {
      truncated = true;
      break;
    }
    const fields = parseCsvLine(lines[index]);
    const values = [];
    let valid = true;
    for (let channel = 0; channel < channelNames.length; channel += 1) {
      const value = Number(fields[channel]);
      if (!Number.isFinite(value)) {
        valid = false;
        break;
      }
      values.push(value);
    }
    if (!valid) {
      skipped += 1;
      continue;
    }
    const sampleIndex = times.length;
    times.push(sampleIndex * sampleInterval);
    for (let channel = 0; channel < channelNames.length; channel += 1) {
      channels[channel].push(values[channel]);
    }
  }

  if (times.length === 0) {
    throw new Error("No valid metadata CSV samples were parsed.");
  }

  return makeDataset({
    filePath,
    format: "metadata-csv",
    channelNames,
    times,
    channels,
    skippedRows: skipped,
    truncated,
    sampleRateHz: 1 / sampleInterval,
  });
}

function parseStandardCsv(filePath, headers, rows) {
  if (headers.length < 2) {
    throw new Error("Standard CSV requires a time column followed by at least one channel column.");
  }

  const channelNames = headers.slice(1, 129).map((name, index) => name || `CH${index + 1}`);
  const channels = channelNames.map(() => []);
  const times = [];
  let skipped = 0;
  let truncated = false;

  for (const line of rows) {
    if (times.length >= MAX_STANDARD_ROWS) {
      truncated = true;
      break;
    }
    const fields = parseCsvLine(line);
    const time = Number(fields[0]);
    if (!Number.isFinite(time)) {
      skipped += 1;
      continue;
    }
    times.push(time);
    for (let index = 0; index < channelNames.length; index += 1) {
      const value = Number(fields[index + 1]);
      channels[index].push(Number.isFinite(value) ? value : NaN);
    }
  }

  if (times.length === 0) {
    throw new Error("No valid standard CSV samples were parsed.");
  }

  return makeDataset({
    filePath,
    format: "standard",
    channelNames,
    times,
    channels,
    skippedRows: skipped,
    truncated,
    sampleRateHz: estimateSampleRate(times),
  });
}

function parseCloudCsv(filePath, headers, rows, contentColumn, options) {
  const sampleRateHz = Math.max(1, Number(options.sampleRateHz) || 1000);
  const times = [];
  const channels = CLOUD_CHANNEL_NAMES.map(() => []);
  let parsedRecords = 0;
  let skipped = 0;
  let truncated = false;

  for (const line of rows) {
    if (parsedRecords >= MAX_CLOUD_RECORDS) {
      truncated = true;
      break;
    }
    const fields = parseCsvLine(line);
    const raw = (fields[contentColumn] || "").trim();
    if (!raw) {
      skipped += 1;
      continue;
    }

    try {
      const frames = parseCloudRecord(raw);
      for (const frame of frames) {
        const sampleIndex = times.length;
        times.push(sampleIndex / sampleRateHz);
        for (let channel = 0; channel < CLOUD_CHANNEL_NAMES.length; channel += 1) {
          channels[channel].push(frame[channel]);
        }
      }
      parsedRecords += 1;
    } catch {
      skipped += 1;
    }
  }

  if (times.length === 0) {
    throw new Error(`No valid cloud Content records were parsed. Headers: ${headers.join(", ")}`);
  }

  return makeDataset({
    filePath,
    format: "cloud-content",
    channelNames: CLOUD_CHANNEL_NAMES,
    times,
    channels,
    skippedRows: skipped,
    truncated,
    sampleRateHz,
  });
}

function parseDatFile(filePath, buffer) {
  const fixedHeaderSize = DAT_HEADER_FIXED_WORDS * DAT_HEADER_WORD_BYTES;
  if (buffer.length < fixedHeaderSize) {
    throw new Error("DAT file is too small to contain a header.");
  }

  const headerLength = buffer.readUInt32LE(0);
  const sampleRateHz = Math.max(1, buffer.readUInt32LE(8));
  const channelCount = buffer.readUInt32LE(12);
  if (channelCount < 1) {
    throw new Error("DAT file does not declare any channels.");
  }
  if (channelCount > MAX_DAT_CHANNELS) {
    throw new Error(`DAT channel count ${channelCount} exceeds supported maximum ${MAX_DAT_CHANNELS}.`);
  }
  if (headerLength < fixedHeaderSize || headerLength >= buffer.length) {
    throw new Error(`Invalid DAT header length ${headerLength} for ${buffer.length} byte file.`);
  }

  const recordSize = channelCount * 2;
  const availableSamples = Math.floor((buffer.length - headerLength) / recordSize);
  if (availableSamples < 1) {
    throw new Error("DAT file has no sample frames.");
  }

  const channelNames = parseDatChannelNames(buffer.subarray(0, headerLength), channelCount);
  const sampleCount = Math.min(availableSamples, MAX_DAT_SAMPLES);
  const truncated = availableSamples > sampleCount;
  const times = new Array(sampleCount);
  const channels = Array.from({ length: channelCount }, () => new Array(sampleCount));

  for (let sampleIndex = 0; sampleIndex < sampleCount; sampleIndex += 1) {
    times[sampleIndex] = sampleIndex / sampleRateHz;
    const recordOffset = headerLength + sampleIndex * recordSize;
    for (let channel = 0; channel < channelCount; channel += 1) {
      channels[channel][sampleIndex] = buffer.readInt16LE(recordOffset + channel * 2);
    }
  }

  return makeDataset({
    filePath,
    format: "binary-dat",
    channelNames,
    times,
    channels,
    skippedRows: 0,
    truncated,
    sampleRateHz,
  });
}

function parseDatChannelNames(header, channelCount) {
  const namesStart = (DAT_HEADER_FIXED_WORDS + channelCount * 5) * DAT_HEADER_WORD_BYTES;
  const names = [];
  if (namesStart < header.length) {
    let start = namesStart;
    for (let index = namesStart; index <= header.length; index += 1) {
      if (index === header.length || header[index] === 0xff) {
        if (index > start) {
          const name = header.subarray(start, index).toString("utf8").trim();
          if (name) {
            names.push(name);
          }
        }
        start = index + 1;
      }
    }
  }

  return Array.from({ length: channelCount }, (_, index) => names[index] || `CH${index + 1}`);
}

function makeDataset({ filePath, format, channelNames, times, channels, skippedRows, truncated, sampleRateHz }) {
  const duration = Math.max(0, times[times.length - 1] - times[0]);
  const summaries = channels.map((values, index) => {
    let min = Infinity;
    let max = -Infinity;
    let finite = 0;
    for (const value of values) {
      if (Number.isFinite(value)) {
        min = Math.min(min, value);
        max = Math.max(max, value);
        finite += 1;
      }
    }
    return {
      index,
      name: channelNames[index],
      visible: index < 8,
      min: finite ? min : 0,
      max: finite ? max : 0,
    };
  });

  return {
    name: path.basename(filePath),
    path: filePath,
    format,
    sampleRateHz,
    sampleCount: times.length,
    duration,
    skippedRows,
    truncated,
    times,
    channels,
    channelSummaries: summaries,
  };
}

function parseCloudRecord(raw) {
  const frameLen = hexByteAt(raw, 6);
  const sublength = frameLen - 5;
  if (sublength <= 0 || sublength % 2 !== 0) {
    throw new Error("Invalid cloud frame length.");
  }

  const wordCount = sublength / 2;
  if (wordCount !== 32) {
    throw new Error(`Invalid cloud word count ${wordCount}; expected 32.`);
  }

  const frame1Start = 18;
  const frame2Start = frame1Start + wordCount * 4 + 12;
  return [
    expandCloudWords(parseCloudWords(raw, frame1Start, wordCount)),
    expandCloudWords(parseCloudWords(raw, frame2Start, wordCount)),
  ];
}

function parseCloudWords(raw, start, wordCount) {
  const words = [];
  for (let index = 0; index < wordCount; index += 1) {
    const pos = start + index * 4;
    const low = hexByteAt(raw, pos);
    const high = hexByteAt(raw, pos + 2);
    words.push(low + high * 256);
  }
  return words;
}

function expandCloudWords(words) {
  const values = new Array(60).fill(0);
  for (let channel = 0; channel < 30; channel += 1) {
    values[channel] = signedWord(words[channel]);
  }

  const hex1 = words[30];
  const hex2 = words[31];
  values[30] = hex1 & 0x0007;

  let bit = 3;
  for (let channel = 31; channel < 43; channel += 1) {
    values[channel] = (hex1 >> bit) & 1;
    bit += 1;
  }

  for (let channel = 43; channel < 60; channel += 1) {
    if (bit > 15) {
      bit = 0;
    }
    values[channel] = (hex2 >> bit) & 1;
    bit += 1;
  }

  return values;
}

function signedWord(raw) {
  return raw > 0x7fff ? raw - 0x10000 : raw;
}

function hexByteAt(raw, charIndex) {
  const pair = raw.slice(charIndex, charIndex + 2);
  if (pair.length !== 2) {
    throw new Error("Cloud record is shorter than expected.");
  }
  if (!/^[0-9a-fA-F]{2}$/.test(pair)) {
    throw new Error(`Invalid hexadecimal byte ${pair}.`);
  }
  const value = Number.parseInt(pair, 16);
  return value;
}

function parseCsvLine(line) {
  const fields = [];
  let field = "";
  let quoted = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (char === '"') {
      if (quoted && line[index + 1] === '"') {
        field += '"';
        index += 1;
      } else {
        quoted = !quoted;
      }
      continue;
    }
    if (char === "," && !quoted) {
      fields.push(field);
      field = "";
      continue;
    }
    field += char;
  }
  fields.push(field);
  return fields.map((value) => value.trim());
}

function estimateSampleRate(times) {
  let sum = 0;
  let count = 0;
  for (let index = 1; index < times.length; index += 1) {
    const dt = times[index] - times[index - 1];
    if (Number.isFinite(dt) && dt > 0) {
      sum += dt;
      count += 1;
    }
  }
  return count ? 1 / (sum / count) : 1;
}

function getWebviewHtml(context, webview) {
  const nonce = getNonce();
  const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(context.extensionUri, "media", "scopeView.js"));
  const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(context.extensionUri, "media", "scopeView.css"));
  const csp = [
    "default-src 'none'",
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `script-src 'nonce-${nonce}'`,
    `img-src ${webview.cspSource} data:`,
  ].join("; ");

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="${csp}">
  <link rel="stylesheet" href="${styleUri}">
  <title>Scope Analyzer</title>
</head>
<body>
  <div id="app">
    <header class="topbar">
      <div class="titleBlock">
        <div class="title">
          <span id="datasetName">Scope Analyzer</span>
          <small id="datasetMeta">No dataset loaded</small>
        </div>
        <div class="datasetStats" aria-label="Dataset summary">
          <span id="formatBadge">--</span>
          <span id="sampleBadge">-- samples</span>
          <span id="durationBadge">-- s</span>
          <span id="channelBadge">-- channels</span>
        </div>
      </div>
      <div class="controls">
        <label class="field">Fs<input id="sampleRate" type="number" min="1" step="100" value="1000"></label>
        <label class="field">Base<input id="harmonicBase" type="number" min="1" step="1" value="50"></label>
        <div class="buttonGroup">
          <select id="exportFormat" title="Export format">
            <option value="standard-csv">CSV</option>
            <option value="data-csv">DATA CSV</option>
            <option value="tsv">TSV</option>
            <option value="json">JSON</option>
          </select>
          <button id="exportButton" type="button">Export</button>
        </div>
        <div class="buttonGroup compact">
          <button id="reloadButton" type="button">Reload</button>
          <button id="resetButton" type="button">Fit</button>
        </div>
      </div>
    </header>
    <main class="workspace">
      <aside class="sidebar">
        <div class="panelHeader">
          <h2>Channels</h2>
          <span id="visibleCount">0 selected</span>
        </div>
        <input id="channelSearch" type="search" placeholder="Search channels">
        <div class="channelActions">
          <button id="selectAll" type="button">All</button>
          <button id="selectNone" type="button">None</button>
          <button id="firstEight" type="button">First 8</button>
        </div>
        <div id="channelList" class="channelList"></div>
      </aside>
      <section class="viewer">
        <div class="viewerHeader">
          <span>Waveform</span>
          <span id="viewRange">--</span>
        </div>
        <canvas id="plot"></canvas>
        <div id="status" class="status">Loading...</div>
      </section>
      <aside class="analysis">
        <div class="panelHeader">
          <h2>Analysis</h2>
          <span id="analysisState">Idle</span>
        </div>
        <section class="analysisSection">
          <h2>Cursors</h2>
          <div id="cursorReadout" class="readout">X1 -- | X2 --</div>
        </section>
        <section class="analysisSection">
          <h2>Measurements</h2>
          <div id="measurements" class="table"></div>
        </section>
        <section class="analysisSection">
          <h2>FFT / THD</h2>
          <label class="wideField">Channel<select id="fftChannel"></select></label>
          <div id="fftReadout" class="table"></div>
        </section>
      </aside>
    </main>
  </div>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
}

function getNonce() {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let nonce = "";
  for (let index = 0; index < 32; index += 1) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}

module.exports = {
  activate,
  deactivate,
  _test: {
    parseWaveformFile,
    parseDatFile,
    parseMetadataCsv,
    parseCsvLine,
    parseCloudRecord,
  },
};
