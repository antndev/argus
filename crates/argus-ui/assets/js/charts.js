window.argusCharts = (() => {
  const charts = new Map();
  let nextId = 1;
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const pad = (n) => String(n).padStart(2, "0");
  const token = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
  const formats = {
    pct: (v) => `${v.toFixed(1)}%`,
    deg: (v) => `${v.toFixed(1)} °C`,
    rate: (v) => (v < 1 ? `${Math.round(v * 1000)} kbit/s` : v < 100 ? `${v.toFixed(1)} Mbit/s` : `${Math.round(v)} Mbit/s`),
    load: (v) => v.toFixed(2),
  };

  function niceCeil(value) {
    if (value <= 0) return 1;
    const power = Math.pow(10, Math.floor(Math.log10(value)));
    const n = value / power;
    const step = [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10].find((s) => n <= s);
    return step * power;
  }

  function yRange(spec, low, high) {
    if (spec.max != null) return [0, spec.max];
    let max = Math.max(high * 1.15, spec.floor || 0);
    for (const t of spec.thresholds) max = Math.max(max, t.v * 1.06);
    const min = spec.unit === "deg" ? Math.max(0, Math.floor((low - 8) / 10) * 10) : 0;
    return [min, niceCeil(max)];
  }

  function stamp(seconds) {
    const date = new Date(seconds * 1000);
    const today = new Date();
    const day = date.toDateString() === today.toDateString() ? "Today" : `${date.getDate()} ${months[date.getMonth()]}`;
    return `${day}, ${pad(date.getHours())}:${pad(date.getMinutes())}`;
  }

  function xValues(u, splits, axis, space, increment) {
    return splits.map((s) => {
      if (s == null) return "";
      const date = new Date(s * 1000);
      return increment >= 86400 ? `${date.getDate()} ${months[date.getMonth()]}` : `${pad(date.getHours())}:${pad(date.getMinutes())}`;
    });
  }

  function yValues(u, splits) {
    return splits.map((v) => (Number.isInteger(v) ? String(v) : v.toFixed(Math.abs(v) < 1 ? 2 : 1)));
  }

  function drawThresholds(u, spec) {
    if (!spec.thresholds.length) return;
    const ctx = u.ctx;
    const dpr = window.devicePixelRatio || 1;
    const { left, top, width, height } = u.bbox;
    ctx.save();
    ctx.font = `${11 * dpr}px Inter, system-ui, sans-serif`;
    ctx.textAlign = "right";
    let previous = -Infinity;
    for (const t of [...spec.thresholds].sort((a, b) => b.v - a.v)) {
      const y = Math.round(u.valToPos(t.v, "y", true)) + 0.5;
      if (y < top || y > top + height) continue;
      const below = y - previous < 16 * dpr;
      ctx.textBaseline = below ? "top" : "bottom";
      previous = y;
      ctx.strokeStyle = token("--warn");
      ctx.lineWidth = dpr;
      ctx.setLineDash([4 * dpr, 4 * dpr]);
      ctx.beginPath();
      ctx.moveTo(left, y);
      ctx.lineTo(left + width, y);
      ctx.stroke();
      ctx.setLineDash([]);
      if (t.label) {
        ctx.fillStyle = token("--text-2");
        ctx.fillText(t.label, left + width - 4 * dpr, below ? y + 3 * dpr : y - 3 * dpr);
      }
    }
    ctx.restore();
  }

  function escape(text) {
    return String(text).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  }

  function tooltip(spec, colors) {
    let tip;
    const format = formats[spec.unit] || formats.load;
    return {
      hooks: {
        init: (u) => {
          tip = document.createElement("div");
          tip.className = "chart-tip";
          tip.hidden = true;
          u.over.appendChild(tip);
        },
        setCursor: (u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) {
            tip.hidden = true;
            return;
          }
          const rows = spec.series
            .map((label, j) => {
              const value = u.data[j + 1][i];
              return `<div class="chart-tip-row"><i style="background:${colors[j]}"></i><span>${escape(label)}</span><b>${value == null ? "n/a" : format(value)}</b></div>`;
            })
            .join("");
          tip.innerHTML = `<div class="chart-tip-time">${stamp(u.data[0][i])}</div>${rows}`;
          tip.hidden = false;
          const width = u.over.clientWidth;
          const height = u.over.clientHeight;
          const x = u.cursor.left;
          tip.style.left = `${x + 14 + tip.offsetWidth > width ? Math.max(0, x - 14 - tip.offsetWidth) : x + 14}px`;
          tip.style.top = `${Math.min(Math.max(0, u.cursor.top - tip.offsetHeight / 2), Math.max(0, height - tip.offsetHeight))}px`;
        },
      },
    };
  }

  function options(entry) {
    const { spec, element } = entry;
    const colors = ["--s1", "--s2", "--s3", "--s4", "--s5", "--s6", "--s7", "--s8"].map(token);
    const axis = token("--text-3");
    const grid = token("--grid");
    const surface = token("--bg");
    const font = "11px Inter, system-ui, sans-serif";
    return {
      width: Math.max(200, element.clientWidth),
      height: element.clientHeight || 180,
      padding: [10, 8, 0, 0],
      legend: { show: false },
      select: { show: false },
      cursor: {
        y: false,
        drag: { x: false, y: false, setScale: false },
        points: { size: 8, width: 2, stroke: () => surface, fill: (u, i) => colors[i - 1] },
      },
      scales: { x: { time: true }, y: { range: (u, low, high) => yRange(spec, low, high) } },
      axes: [
        { stroke: axis, font, size: 28, space: 76, gap: 6, ticks: { show: false }, grid: { stroke: grid, width: 1 }, values: xValues },
        { stroke: axis, font, size: 42, space: 34, gap: 6, ticks: { show: false }, grid: { stroke: grid, width: 1 }, values: yValues },
      ],
      series: [
        {},
        ...spec.series.map((label, j) => ({
          label,
          stroke: colors[j],
          width: 2,
          fill: spec.series.length === 1 ? `${colors[j]}1f` : undefined,
          points: { show: false },
        })),
      ],
      hooks: { draw: [(u) => drawThresholds(u, spec)] },
      plugins: [tooltip(spec, colors)],
    };
  }

  function mount(entry, animate) {
    entry.plot = new uPlot(options(entry), entry.data, entry.element);
    if (!animate) entry.plot.root.classList.add("no-anim");
  }

  function columns(json) {
    const series = JSON.parse(json);
    return [series.times.map((t) => t / 1000), ...series.values];
  }

  function create(element, specJson, dataJson) {
    if (!window.uPlot) throw new Error("uPlot is not loaded");
    const entry = { element, spec: JSON.parse(specJson), data: columns(dataJson), plot: null, observer: null };
    mount(entry, true);
    entry.observer = new ResizeObserver(() => {
      const width = element.clientWidth;
      if (width && entry.plot && Math.abs(width - entry.plot.width) > 1) entry.plot.setSize({ width, height: entry.plot.height });
    });
    entry.observer.observe(element);
    const id = nextId++;
    charts.set(id, entry);
    return id;
  }

  function update(id, dataJson, animate) {
    const entry = charts.get(id);
    if (!entry) return;
    entry.data = columns(dataJson);
    entry.plot.setData(entry.data);
    if (animate && !reducedMotion()) {
      entry.plot.root.animate([{ opacity: 0.15, transform: "translateY(4px)" }, { opacity: 1, transform: "none" }], { duration: 360, easing: "cubic-bezier(.2,.9,.3,1)" });
    }
  }

  function destroy(id) {
    const entry = charts.get(id);
    if (!entry) return;
    entry.observer.disconnect();
    entry.plot.destroy();
    charts.delete(id);
  }

  function retheme() {
    for (const entry of charts.values()) {
      entry.plot.destroy();
      mount(entry, false);
    }
  }

  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", retheme);
  new MutationObserver(retheme).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

  return { create, update, destroy };
})();
