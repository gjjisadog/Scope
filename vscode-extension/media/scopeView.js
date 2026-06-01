const vscode = acquireVsCodeApi();

const state = {
  dataset: null,
  visible: new Set(),
  filter: "",
  viewStart: 0,
  viewEnd: 1,
  cursors: [null, null],
  nextCursor: 0,
  fftRequestId: 0,
  dragging: false,
  dragX: 0,
  dragStart: 0,
  dragEnd: 1,
  colors: [
    "#4cc2ff",
    "#ffb454",
    "#73c991",
    "#f88070",
    "#c586c0",
    "#dcdcaa",
    "#9cdcfe",
    "#ce9178",
    "#b5cea8",
    "#d7ba7d",
  ],
};

const els = {
  datasetName: document.getElementById("datasetName"),
  datasetMeta: document.getElementById("datasetMeta"),
  formatBadge: document.getElementById("formatBadge"),
  sampleBadge: document.getElementById("sampleBadge"),
  durationBadge: document.getElementById("durationBadge"),
  channelBadge: document.getElementById("channelBadge"),
  sampleRate: document.getElementById("sampleRate"),
  harmonicBase: document.getElementById("harmonicBase"),
  exportFormat: document.getElementById("exportFormat"),
  exportButton: document.getElementById("exportButton"),
  reloadButton: document.getElementById("reloadButton"),
  resetButton: document.getElementById("resetButton"),
  channelSearch: document.getElementById("channelSearch"),
  visibleCount: document.getElementById("visibleCount"),
  selectAll: document.getElementById("selectAll"),
  selectNone: document.getElementById("selectNone"),
  firstEight: document.getElementById("firstEight"),
  channelList: document.getElementById("channelList"),
  viewRange: document.getElementById("viewRange"),
  canvas: document.getElementById("plot"),
  status: document.getElementById("status"),
  analysisState: document.getElementById("analysisState"),
  cursorReadout: document.getElementById("cursorReadout"),
  measurements: document.getElementById("measurements"),
  fftChannel: document.getElementById("fftChannel"),
  fftReadout: document.getElementById("fftReadout"),
};

const ctx = els.canvas.getContext("2d");

window.addEventListener("message", (event) => {
  const message = event.data;
  if (message.type === "loading") {
    els.status.textContent = `Loading ${message.path}`;
  }
  if (message.type === "error") {
    els.status.textContent = message.message;
  }
  if (message.type === "dataset") {
    setDataset(message.dataset);
  }
  if (message.type === "fft") {
    renderFftResult(message);
  }
});

window.addEventListener("resize", () => draw());

els.reloadButton.addEventListener("click", () => {
  vscode.postMessage({ type: "reload", options: readOptions() });
});

els.exportButton.addEventListener("click", () => {
  if (!state.dataset) {
    return;
  }
  vscode.postMessage({
    type: "export",
    format: els.exportFormat.value,
    options: readOptions(),
  });
});

els.resetButton.addEventListener("click", () => fitView());
els.channelSearch.addEventListener("input", () => {
  state.filter = els.channelSearch.value.trim().toLowerCase();
  renderChannelList();
});
els.selectAll.addEventListener("click", () => setVisible("all"));
els.selectNone.addEventListener("click", () => setVisible("none"));
els.firstEight.addEventListener("click", () => setVisible("firstEight"));
els.fftChannel.addEventListener("change", () => renderAnalysis());

els.canvas.addEventListener("click", (event) => {
  if (!state.dataset || state.dragging) {
    return;
  }
  const time = pixelToTime(event.offsetX);
  state.cursors[state.nextCursor] = clamp(time, state.dataset.times[0], state.dataset.times[state.dataset.times.length - 1]);
  state.nextCursor = state.nextCursor === 0 ? 1 : 0;
  renderAnalysis();
  draw();
});

els.canvas.addEventListener("wheel", (event) => {
  if (!state.dataset) {
    return;
  }
  event.preventDefault();
  const center = pixelToTime(event.offsetX);
  const factor = event.deltaY < 0 ? 0.82 : 1.22;
  zoomAround(center, factor);
});

els.canvas.addEventListener("mousedown", (event) => {
  if (event.button !== 1 && event.button !== 2) {
    return;
  }
  event.preventDefault();
  state.dragging = true;
  state.dragX = event.clientX;
  state.dragStart = state.viewStart;
  state.dragEnd = state.viewEnd;
});

window.addEventListener("mousemove", (event) => {
  if (!state.dragging || !state.dataset) {
    return;
  }
  const width = Math.max(1, els.canvas.clientWidth);
  const dt = ((event.clientX - state.dragX) / width) * (state.dragEnd - state.dragStart);
  setView(state.dragStart - dt, state.dragEnd - dt);
});

window.addEventListener("mouseup", () => {
  state.dragging = false;
});

els.canvas.addEventListener("contextmenu", (event) => event.preventDefault());

function setDataset(dataset) {
  state.dataset = dataset;
  state.visible = new Set(
    dataset.channelSummaries.filter((channel) => channel.visible).map((channel) => channel.index)
  );
  state.cursors = [null, null];
  state.nextCursor = 0;
  els.sampleRate.value = String(Math.round(dataset.sampleRateHz || 1000));
  els.datasetName.textContent = dataset.name;
  els.datasetMeta.textContent = dataset.path;
  els.formatBadge.textContent = dataset.format;
  els.sampleBadge.textContent = `${formatNumber(dataset.sampleCount)} samples`;
  els.durationBadge.textContent = `${formatNumber(dataset.duration)} s`;
  els.channelBadge.textContent = `${dataset.channelSummaries.length} channels`;
  if (dataset.skippedRows || dataset.truncated) {
    const parts = [];
    if (dataset.skippedRows) {
      parts.push(`${dataset.skippedRows} skipped rows`);
    }
    if (dataset.truncated) {
      parts.push("loaded with safety limit");
    }
    els.status.textContent = parts.join(" | ");
  } else {
    els.status.textContent = "Ready";
  }
  fitView(false);
  renderChannelList();
  updateVisibleCount();
  updateViewRange();
  renderFftSelector();
  renderAnalysis();
  draw();
}

function readOptions() {
  return {
    sampleRateHz: Number(els.sampleRate.value) || 1000,
  };
}

function setVisible(mode) {
  if (!state.dataset) {
    return;
  }
  if (mode === "all") {
    state.visible = new Set(state.dataset.channelSummaries.map((channel) => channel.index));
  }
  if (mode === "none") {
    state.visible = new Set();
  }
  if (mode === "firstEight") {
    state.visible = new Set(state.dataset.channelSummaries.slice(0, 8).map((channel) => channel.index));
  }
  renderChannelList();
  renderFftSelector();
  updateVisibleCount();
  renderAnalysis();
  draw();
}

function renderChannelList() {
  if (!state.dataset) {
    els.channelList.innerHTML = "";
    return;
  }
  const query = state.filter;
  els.channelList.innerHTML = "";
  for (const channel of state.dataset.channelSummaries) {
    if (query && !channel.name.toLowerCase().includes(query) && !String(channel.index + 1).includes(query)) {
      continue;
    }
    const item = document.createElement("label");
    item.className = "channelItem";
    item.title = channel.name;

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = state.visible.has(channel.index);
    item.classList.toggle("selected", checkbox.checked);
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) {
        state.visible.add(channel.index);
      } else {
        state.visible.delete(channel.index);
      }
      item.classList.toggle("selected", checkbox.checked);
      renderFftSelector();
      updateVisibleCount();
      renderAnalysis();
      draw();
    });

    const swatch = document.createElement("span");
    swatch.className = "swatch";
    swatch.style.background = colorFor(channel.index);

    const name = document.createElement("span");
    name.className = "channelName";
    name.textContent = `${channel.index + 1}. ${channel.name}`;

    item.append(checkbox, swatch, name);
    els.channelList.append(item);
  }
  if (!els.channelList.children.length) {
    const empty = document.createElement("div");
    empty.className = "emptyState";
    empty.textContent = "No channels";
    els.channelList.append(empty);
  }
}

function renderFftSelector() {
  if (!state.dataset) {
    return;
  }
  const previous = Number(els.fftChannel.value);
  els.fftChannel.innerHTML = "";
  const channels = [...state.visible];
  if (!channels.length) {
    state.dataset.channelSummaries.slice(0, 8).forEach((channel) => channels.push(channel.index));
  }
  for (const channelIndex of channels) {
    const option = document.createElement("option");
    option.value = String(channelIndex);
    option.textContent = state.dataset.channelSummaries[channelIndex].name;
    els.fftChannel.append(option);
  }
  if (Number.isInteger(previous) && channels.includes(previous)) {
    els.fftChannel.value = String(previous);
  }
}

function fitView(render = true) {
  if (!state.dataset) {
    return;
  }
  state.viewStart = state.dataset.times[0];
  state.viewEnd = state.dataset.times[state.dataset.times.length - 1];
  if (state.viewEnd <= state.viewStart) {
    state.viewEnd = state.viewStart + 1;
  }
  if (render) {
    updateViewRange();
    draw();
  }
}

function setView(start, end) {
  if (!state.dataset) {
    return;
  }
  const min = state.dataset.times[0];
  const max = state.dataset.times[state.dataset.times.length - 1];
  const width = end - start;
  if (width >= max - min) {
    state.viewStart = min;
    state.viewEnd = max;
  } else {
    if (start < min) {
      end += min - start;
      start = min;
    }
    if (end > max) {
      start -= end - max;
      end = max;
    }
    state.viewStart = start;
    state.viewEnd = Math.max(start + Number.EPSILON, end);
  }
  updateViewRange();
  draw();
}

function zoomAround(center, factor) {
  const start = center - (center - state.viewStart) * factor;
  const end = center + (state.viewEnd - center) * factor;
  setView(start, end);
}

function draw() {
  resizeCanvas();
  const dataset = state.dataset;
  ctx.clearRect(0, 0, els.canvas.width, els.canvas.height);
  if (!dataset) {
    return;
  }
  updateViewRange();

  const width = els.canvas.width;
  const height = els.canvas.height;
  const padding = { left: 52, right: 16, top: 18, bottom: 32 };
  const plot = {
    x: padding.left,
    y: padding.top,
    width: Math.max(1, width - padding.left - padding.right),
    height: Math.max(1, height - padding.top - padding.bottom),
  };

  ctx.fillStyle = getCss("--vscode-editor-background", "#1e1e1e");
  ctx.fillRect(0, 0, width, height);
  drawGrid(plot);

  const visibleChannels = [...state.visible];
  const range = getYRange(visibleChannels);
  for (const channelIndex of visibleChannels) {
    drawChannel(channelIndex, plot, range.min, range.max);
  }

  drawAxes(plot, range.min, range.max);
  drawCursors(plot);
}

function drawGrid(plot) {
  ctx.save();
  ctx.strokeStyle = getCss("--vscode-panel-border", "#3a3a3a");
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let i = 0; i <= 10; i += 1) {
    const x = plot.x + (plot.width * i) / 10;
    ctx.moveTo(x, plot.y);
    ctx.lineTo(x, plot.y + plot.height);
  }
  for (let i = 0; i <= 8; i += 1) {
    const y = plot.y + (plot.height * i) / 8;
    ctx.moveTo(plot.x, y);
    ctx.lineTo(plot.x + plot.width, y);
  }
  ctx.stroke();
  ctx.restore();
}

function drawAxes(plot, minY, maxY) {
  ctx.save();
  ctx.fillStyle = getCss("--vscode-descriptionForeground", "#999");
  ctx.font = "11px sans-serif";
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  for (let i = 0; i <= 4; i += 1) {
    const ratio = i / 4;
    const value = maxY - (maxY - minY) * ratio;
    const y = plot.y + plot.height * ratio;
    ctx.fillText(formatNumber(value), plot.x - 8, y);
  }
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  for (let i = 0; i <= 5; i += 1) {
    const ratio = i / 5;
    const value = state.viewStart + (state.viewEnd - state.viewStart) * ratio;
    const x = plot.x + plot.width * ratio;
    ctx.fillText(formatNumber(value), x, plot.y + plot.height + 8);
  }
  ctx.restore();
}

function drawChannel(channelIndex, plot, minY, maxY) {
  const dataset = state.dataset;
  const times = dataset.times;
  const values = dataset.channels[channelIndex];
  const startIndex = lowerBound(times, state.viewStart);
  const endIndex = Math.min(values.length - 1, upperBound(times, state.viewEnd));
  if (endIndex <= startIndex) {
    return;
  }

  ctx.save();
  ctx.strokeStyle = colorFor(channelIndex);
  ctx.lineWidth = 1.35;
  ctx.beginPath();

  const points = endIndex - startIndex + 1;
  if (points > plot.width * 2) {
    const samplesPerPixel = Math.max(1, Math.floor(points / plot.width));
    for (let index = startIndex; index <= endIndex; index += samplesPerPixel) {
      let min = Infinity;
      let max = -Infinity;
      let time = times[index];
      const limit = Math.min(endIndex + 1, index + samplesPerPixel);
      for (let i = index; i < limit; i += 1) {
        const value = values[i];
        if (Number.isFinite(value)) {
          min = Math.min(min, value);
          max = Math.max(max, value);
        }
      }
      if (Number.isFinite(min) && Number.isFinite(max)) {
        const x = timeToPixel(time, plot);
        ctx.moveTo(x, valueToPixel(min, plot, minY, maxY));
        ctx.lineTo(x, valueToPixel(max, plot, minY, maxY));
      }
    }
  } else {
    let started = false;
    for (let index = startIndex; index <= endIndex; index += 1) {
      const value = values[index];
      if (!Number.isFinite(value)) {
        started = false;
        continue;
      }
      const x = timeToPixel(times[index], plot);
      const y = valueToPixel(value, plot, minY, maxY);
      if (!started) {
        ctx.moveTo(x, y);
        started = true;
      } else {
        ctx.lineTo(x, y);
      }
    }
  }
  ctx.stroke();
  ctx.restore();
}

function drawCursors(plot) {
  ctx.save();
  ctx.setLineDash([5, 4]);
  ctx.lineWidth = 1;
  ctx.font = "12px sans-serif";
  ctx.textBaseline = "top";
  state.cursors.forEach((cursor, index) => {
    if (cursor === null) {
      return;
    }
    const x = timeToPixel(cursor, plot);
    ctx.strokeStyle = index === 0 ? "#ffffff" : "#ffcc66";
    ctx.fillStyle = ctx.strokeStyle;
    ctx.beginPath();
    ctx.moveTo(x, plot.y);
    ctx.lineTo(x, plot.y + plot.height);
    ctx.stroke();
    ctx.fillText(`X${index + 1}`, x + 5, plot.y + 4 + index * 16);
  });
  ctx.restore();
}

function getYRange(channelIndexes) {
  let min = Infinity;
  let max = -Infinity;
  const startIndex = lowerBound(state.dataset.times, state.viewStart);
  const endIndex = upperBound(state.dataset.times, state.viewEnd);
  for (const channelIndex of channelIndexes) {
    const values = state.dataset.channels[channelIndex];
    const step = Math.max(1, Math.floor((endIndex - startIndex) / 5000));
    for (let index = startIndex; index <= endIndex; index += step) {
      const value = values[index];
      if (Number.isFinite(value)) {
        min = Math.min(min, value);
        max = Math.max(max, value);
      }
    }
  }
  if (!Number.isFinite(min) || !Number.isFinite(max) || min === max) {
    min = -1;
    max = 1;
  }
  const pad = (max - min) * 0.08 || 1;
  return { min: min - pad, max: max + pad };
}

function renderAnalysis() {
  if (!state.dataset) {
    return;
  }
  renderCursors();
  renderMeasurements();
  renderFft();
  els.analysisState.textContent = state.visible.size ? `${state.visible.size} active` : "No channels";
}

function renderCursors() {
  const [x1, x2] = state.cursors;
  if (x1 === null && x2 === null) {
    els.cursorReadout.textContent = "X1 -- | X2 --";
    return;
  }
  const parts = [];
  if (x1 !== null) {
    parts.push(`X1 ${formatNumber(x1)} s`);
  }
  if (x2 !== null) {
    parts.push(`X2 ${formatNumber(x2)} s`);
  }
  if (x1 !== null && x2 !== null) {
    parts.push(`dX ${formatNumber(Math.abs(x2 - x1))} s`);
  }
  els.cursorReadout.textContent = parts.join(" | ");
}

function renderMeasurements() {
  const [x1, x2] = orderedCursors();
  els.measurements.innerHTML = "";
  if (x1 === null || x2 === null) {
    appendRow(els.measurements, "Range", "Set X1/X2");
    return;
  }
  const startIndex = lowerBound(state.dataset.times, x1);
  const endIndex = upperBound(state.dataset.times, x2);
  for (const channelIndex of [...state.visible].slice(0, 10)) {
    const name = state.dataset.channelSummaries[channelIndex].name;
    const values = state.dataset.channels[channelIndex];
    const y1 = nearestValue(channelIndex, x1);
    const y2 = nearestValue(channelIndex, x2);
    let min = Infinity;
    let max = -Infinity;
    for (let index = startIndex; index <= endIndex; index += 1) {
      const value = values[index];
      if (Number.isFinite(value)) {
        min = Math.min(min, value);
        max = Math.max(max, value);
      }
    }
    appendRow(els.measurements, name, `dY ${formatNumber(y2 - y1)} | min ${formatNumber(min)} | max ${formatNumber(max)}`);
  }
}

function renderFft() {
  els.fftReadout.innerHTML = "";
  const channelIndex = Number(els.fftChannel.value);
  if (!state.dataset || !Number.isInteger(channelIndex)) {
    appendRow(els.fftReadout, "FFT", "No channel");
    return;
  }
  const [x1, x2] = orderedCursors();
  const start = x1 ?? state.viewStart;
  const end = x2 ?? state.viewEnd;
  if (state.dataset.bridgeAvailable) {
    const requestId = ++state.fftRequestId;
    appendRow(els.fftReadout, "FFT", "Calculating...");
    vscode.postMessage({
      type: "fft",
      requestId,
      options: {
        channel: channelIndex,
        start,
        end,
        sampleRateHz: Number(els.sampleRate.value) || state.dataset.sampleRateHz,
        harmonicBaseHz: Number(els.harmonicBase.value) || 50,
      },
    });
    return;
  }

  const startIndex = lowerBound(state.dataset.times, start);
  const endIndex = upperBound(state.dataset.times, end);
  const samples = state.dataset.channels[channelIndex].slice(startIndex, endIndex + 1);
  const result = analyzeHarmonics(samples, Number(els.sampleRate.value) || state.dataset.sampleRateHz, Number(els.harmonicBase.value) || 50, 10);
  if (!result) {
    appendRow(els.fftReadout, "FFT", "Need at least 16 samples");
    return;
  }
  appendFftRows(result);
}

function renderFftResult(message) {
  if (message.requestId !== state.fftRequestId) {
    return;
  }
  els.fftReadout.innerHTML = "";
  if (message.error) {
    appendRow(els.fftReadout, "FFT", message.error);
    return;
  }
  appendFftRows(message.result);
}

function appendFftRows(result) {
  appendRow(els.fftReadout, "Samples", formatNumber(result.sampleCount));
  appendRow(els.fftReadout, "THD", `${formatNumber(result.thdPercent)}%`);
  for (const row of result.harmonics) {
    appendRow(
      els.fftReadout,
      row.order === 0 ? "DC" : `H${row.order}`,
      `${formatNumber(row.amplitude)} | ${Number.isFinite(row.phaseDeg) ? formatNumber(row.phaseDeg) : "-"} deg | ${formatNumber(row.relativePercent)}%`
    );
  }
}

function analyzeHarmonics(samples, sampleRateHz, baseHz, count) {
  const finite = samples.filter((sample) => Number.isFinite(sample));
  if (finite.length < 16 || sampleRateHz <= 0 || baseHz <= 0) {
    return null;
  }
  const mean = finite.reduce((sum, value) => sum + value, 0) / finite.length;
  const fundamental = harmonicPhasor(samples, sampleRateHz, baseHz, mean);
  if (!fundamental) {
    return null;
  }
  const fundamentalAmp = magnitude(fundamental);
  const dcAmplitude = Math.abs(mean);
  const harmonics = [
    {
      order: 0,
      amplitude: dcAmplitude,
      phaseDeg: NaN,
      relativePercent: fundamentalAmp > 0 ? (dcAmplitude / fundamentalAmp) * 100 : 0,
    },
  ];
  let harmonicPower = 0;
  for (let order = 1; order <= count; order += 1) {
    const phasor = harmonicPhasor(samples, sampleRateHz, baseHz * order, mean);
    if (!phasor) {
      break;
    }
    const amplitude = magnitude(phasor);
    if (order > 1) {
      harmonicPower += amplitude * amplitude;
    }
    harmonics.push({
      order,
      amplitude,
      phaseDeg: Math.atan2(phasor.im, phasor.re) * 180 / Math.PI,
      relativePercent: fundamentalAmp > 0 ? (amplitude / fundamentalAmp) * 100 : 0,
    });
  }
  return {
    sampleCount: finite.length,
    thdPercent: fundamentalAmp > 0 ? (Math.sqrt(harmonicPower) / fundamentalAmp) * 100 : 0,
    harmonics,
  };
}

function harmonicPhasor(samples, sampleRateHz, frequencyHz, mean) {
  if (frequencyHz >= sampleRateHz * 0.5) {
    return null;
  }
  let re = 0;
  let im = 0;
  let windowSum = 0;
  const denom = Math.max(1, samples.length - 1);
  for (let index = 0; index < samples.length; index += 1) {
    const sample = samples[index];
    if (!Number.isFinite(sample)) {
      continue;
    }
    const windowValue = 0.5 - 0.5 * Math.cos((Math.PI * 2 * index) / denom);
    const centered = (sample - mean) * windowValue;
    const angle = (Math.PI * 2 * frequencyHz * index) / sampleRateHz;
    re += centered * Math.cos(angle);
    im -= centered * Math.sin(angle);
    windowSum += windowValue;
  }
  if (windowSum <= Number.EPSILON) {
    return null;
  }
  const scale = 2 / windowSum;
  return { re: re * scale, im: im * scale };
}

function appendRow(parent, name, value) {
  const row = document.createElement("div");
  row.className = "row";
  const nameEl = document.createElement("span");
  nameEl.className = "name";
  nameEl.textContent = name;
  const valueEl = document.createElement("span");
  valueEl.className = "value";
  valueEl.textContent = value;
  row.append(nameEl, valueEl);
  parent.append(row);
}

function updateVisibleCount() {
  if (!state.dataset) {
    els.visibleCount.textContent = "0 selected";
    return;
  }
  els.visibleCount.textContent = `${state.visible.size}/${state.dataset.channelSummaries.length} selected`;
}

function updateViewRange() {
  if (!state.dataset) {
    els.viewRange.textContent = "--";
    return;
  }
  const span = Math.max(0, state.viewEnd - state.viewStart);
  els.viewRange.textContent = `${formatNumber(state.viewStart)} - ${formatNumber(state.viewEnd)} s | span ${formatNumber(span)} s`;
}

function orderedCursors() {
  const [a, b] = state.cursors;
  if (a === null || b === null) {
    return [a, b];
  }
  return a <= b ? [a, b] : [b, a];
}

function nearestValue(channelIndex, time) {
  const index = lowerBound(state.dataset.times, time);
  const left = Math.max(0, index - 1);
  const right = Math.min(state.dataset.times.length - 1, index);
  const nearest = Math.abs(state.dataset.times[left] - time) <= Math.abs(state.dataset.times[right] - time) ? left : right;
  return state.dataset.channels[channelIndex][nearest];
}

function resizeCanvas() {
  const scale = window.devicePixelRatio || 1;
  const width = Math.max(1, Math.floor(els.canvas.clientWidth * scale));
  const height = Math.max(1, Math.floor(els.canvas.clientHeight * scale));
  if (els.canvas.width !== width || els.canvas.height !== height) {
    els.canvas.width = width;
    els.canvas.height = height;
  }
}

function timeToPixel(time, plot = null) {
  const area = plot || { x: 52, width: Math.max(1, els.canvas.width - 68) };
  return area.x + ((time - state.viewStart) / (state.viewEnd - state.viewStart)) * area.width;
}

function pixelToTime(pixel) {
  const scale = window.devicePixelRatio || 1;
  const canvasPixel = pixel * scale;
  const left = 52;
  const width = Math.max(1, els.canvas.width - 68);
  return state.viewStart + clamp((canvasPixel - left) / width, 0, 1) * (state.viewEnd - state.viewStart);
}

function valueToPixel(value, plot, minY, maxY) {
  return plot.y + (1 - (value - minY) / (maxY - minY)) * plot.height;
}

function lowerBound(values, target) {
  let lo = 0;
  let hi = values.length;
  while (lo < hi) {
    const mid = Math.floor((lo + hi) / 2);
    if (values[mid] < target) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  return Math.min(values.length - 1, lo);
}

function upperBound(values, target) {
  let lo = 0;
  let hi = values.length;
  while (lo < hi) {
    const mid = Math.floor((lo + hi) / 2);
    if (values[mid] <= target) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  return Math.max(0, lo - 1);
}

function colorFor(index) {
  return state.colors[index % state.colors.length];
}

function getCss(name, fallback) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

function formatNumber(value) {
  if (!Number.isFinite(value)) {
    return "-";
  }
  const abs = Math.abs(value);
  if ((abs >= 10000 || abs < 0.001) && abs !== 0) {
    return value.toExponential(3);
  }
  return value.toLocaleString(undefined, { maximumFractionDigits: 4 });
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

vscode.postMessage({ type: "ready", options: readOptions() });
