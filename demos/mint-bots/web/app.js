"use strict";

// The control page: status every 5 s, charts every minute, the journal's
// newest page every 15 s unless older pages are open. All text goes in
// through textContent.

const SPARKS_PER_FLM = 1e8;
let config = null;
let oldest = null;

const $ = (selector, root = document) => root.querySelector(selector);
const sats = (value) => value == null ? "—" : `${Math.round(value).toLocaleString("en-US")} sat`;
const flm = (sparks) => sparks == null ? "—" : `${(sparks / SPARKS_PER_FLM).toLocaleString("en-US", { maximumFractionDigits: 2 })} FLM`;
const time = (ms) => new Date(ms).toLocaleString(undefined, { dateStyle: "short", timeStyle: "medium" });
const local = (seconds) => {
  const date = new Date(seconds * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (className) node.className = className;
  return node;
}

async function api(path, options) {
  const response = await fetch(path, options);
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

function stat(list, label, value, bad) {
  const item = element("div");
  item.append(element("dt", label), element("dd", value, bad ? "bad" : null));
  list.append(item);
}

function renderStatus({ config: current, status }) {
  const summary = [
    status.height == null ? "No block seen yet" : `Block ${status.height.toLocaleString("en-US")}`,
    `ledger ${status.epoch}`,
    status.polled_at ? `polled ${time(status.polled_at)}` : "not polled yet",
    `playground ${status.playground}`,
  ];
  if (status.revision) summary.push(`build ${status.revision.slice(0, 7)}`);
  if (status.failure) summary.push(`⚠ ${status.failure.message} (×${status.failure.count} since ${time(status.failure.since)})`);
  $("#summary").textContent = summary.join(" · ");
  $("#summary").classList.toggle("bad", !!status.failure);

  for (const bot of status.bots) {
    const section = $(`[data-bot="${bot.bot}"]`);
    const settings = current[bot.bot];
    const state = $(".state", section);
    state.textContent = settings.enabled ? "running" : "paused";
    state.className = `state ${settings.enabled ? "on" : "off"}`;

    const list = $(".stats", section);
    list.replaceChildren();
    const sample = bot.sample;
    stat(list, "Identity", bot.user_id == null ? "not registered" : `${settings.name} · user #${bot.user_id}`);
    stat(list, "Address", bot.address);
    if (bot.bot === "minimax") {
      stat(list, "Budget / committed", sample ? `${sats(sample.budget_sats)} / ${sats(sample.committed_sats)}` : "—");
      stat(list, "Pace allows by now", sample ? sats(sample.allowance_sats) : "—");
      stat(list, "Next chunk", bot.pace ? sats(bot.pace.chunk) : "—");
      stat(list, "Best window now", bot.best
        ? `${bot.best.price.toFixed(1)} sat/FLM: ${bot.best.duration} blocks in ${bot.best.delay_blocks}`
        : "—");
      stat(list, "Going rate", bot.reference_price == null ? "—" : `${bot.reference_price.toFixed(1)} sat/FLM`);
    } else {
      stat(list, "Committed", sample ? sats(sample.committed_sats) : "—");
    }
    stat(list, "Burned / fees", sample ? `${sats(sample.burned_sats)} / ${sats(sample.fees_sats)}` : "—");
    stat(list, "Earned (incl. commissions)", sample ? flm(sample.reward_sparks) : "—");
    stat(list, "Rate, burned only", sample && sample.reward_sparks > 0
      ? `${(sample.burned_sats / (sample.reward_sparks / SPARKS_PER_FLM)).toFixed(1)} sat/FLM` : "—");
    stat(list, "Own burn now", `${bot.live.own_burn.toLocaleString("en-US")} sat/block`);
    stat(list, "Delegated now", `${bot.live.delegated_burn.toLocaleString("en-US")} sat/block from ${bot.live.delegators}`);
    stat(list, "Delegated, all budgets", sats(bot.live.delegated_budget));
    stat(list, "Everyone's eligible burn", `${bot.live.network_burn.toLocaleString("en-US")} sat/block`);
    if (bot.pending) {
      stat(list, "Pending submission", `${sats(bot.pending.budget_sats)} over ${bot.pending.duration} blocks since ${time(bot.pending.created_at)}`, true);
    }
    if (bot.failure) {
      stat(list, "Failing", `${bot.failure.message} (×${bot.failure.count} since ${time(bot.failure.since)})`, true);
    }
  }

  // Forms follow the server unless someone is editing them.
  const changed = JSON.stringify(current) !== JSON.stringify(config);
  config = current;
  if (changed) {
    for (const form of document.querySelectorAll("form")) {
      if (!form.contains(document.activeElement)) fill(form);
    }
  }
}

function fill(form) {
  const bot = form.closest("[data-bot]").dataset.bot;
  const settings = config[bot];
  for (const input of form.elements) {
    if (!input.name) continue;
    if (input.type === "checkbox") input.checked = settings[input.name];
    else if (input.name === "commission") input.value = settings.commission_bps / 100;
    else if (input.type === "datetime-local") input.value = local(settings[input.name]);
    else input.value = settings[input.name];
  }
}

async function save(event) {
  event.preventDefault();
  const form = event.target;
  const bot = form.closest("[data-bot]").dataset.bot;
  const settings = { ...config[bot] };
  for (const input of form.elements) {
    if (!input.name) continue;
    if (input.type === "checkbox") settings[input.name] = input.checked;
    else if (input.name === "commission") settings.commission_bps = Math.round(Number(input.value) * 100);
    else if (input.type === "datetime-local") settings[input.name] = Math.floor(new Date(input.value).getTime() / 1000);
    else if (input.type === "number") settings[input.name] = Number(input.value);
    else settings[input.name] = input.value.trim();
  }
  const output = $("output", form);
  output.className = "";
  output.textContent = "Saving…";
  try {
    config = await api("/api/config", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ...config, [bot]: settings }),
    });
    fill(form);
    output.textContent = `Saved ${new Date().toLocaleTimeString()}`;
    refreshEvents();
  } catch (error) {
    output.className = "bad";
    output.textContent = error.message;
  }
}

function chart(title, times, series) {
  const box = element("div", null, "chart");
  box.append(element("h3", title));
  const values = series.flatMap((line) => line.values).filter((value) => value != null);
  const min = Math.min(0, ...values);
  const max = Math.max(1, ...values);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 1000 100");
  svg.setAttribute("preserveAspectRatio", "none");
  const [start, end] = [times[0], times[times.length - 1]];
  for (const [index, { values: line }] of series.entries()) {
    const points = line
      .map((value, at) => value == null ? null
        : `${((times[at] - start) / Math.max(1, end - start) * 1000).toFixed(1)},${(100 - (value - min) / (max - min) * 98 - 1).toFixed(1)}`)
      .filter(Boolean);
    const polyline = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    polyline.setAttribute("points", points.join(" "));
    polyline.setAttribute("class", `s${index + 1}`);
    svg.append(polyline);
  }
  box.append(svg);
  const scale = element("div", null, "scale");
  scale.append(element("span", times.length ? time(start) : "no samples yet"),
    element("span", `max ${series[0].format(max)}`),
    element("span", times.length ? time(end) : ""));
  box.append(scale);
  if (series.length > 1) {
    const legend = element("div", null, "legend");
    series.forEach(({ label }, index) => legend.append(element("span", `— ${label}`, `s${index + 1}`)));
    box.append(legend);
  }
  return box;
}

async function refreshCharts() {
  const range = Number($("#range").value);
  const since = range ? Date.now() - range : 0;
  for (const bot of ["clock", "minimax"]) {
    let samples;
    try {
      samples = await api(`/api/samples?bot=${bot}&since=${since}`);
    } catch {
      continue;
    }
    const times = samples.map((sample) => sample.at);
    const pick = (field) => samples.map((sample) => sample[field]);
    const satsOf = (value) => sats(value);
    const flmOf = (value) => flm(value);
    const perBlock = (value) => `${Math.round(value).toLocaleString("en-US")} sat/block`;
    const charts = bot === "minimax"
      ? [
        chart("Spending", times, [
          { label: "committed", values: pick("committed_sats"), format: satsOf },
          { label: "pace allows", values: pick("allowance_sats"), format: satsOf },
          { label: "budget", values: pick("budget_sats"), format: satsOf },
        ]),
        chart("Earned", times, [{ label: "earned", values: pick("reward_sparks"), format: flmOf }]),
        chart("Burn per block", times, [
          { label: "own", values: pick("own_burn"), format: perBlock },
          { label: "delegated to it", values: pick("delegated_burn"), format: perBlock },
          { label: "everyone eligible", values: pick("network_burn"), format: perBlock },
        ]),
      ]
      : [
        chart("Committed", times, [{ label: "committed", values: pick("committed_sats"), format: satsOf }]),
        chart("Earned", times, [{ label: "earned", values: pick("reward_sparks"), format: flmOf }]),
        chart("Burn per block", times, [
          { label: "own", values: pick("own_burn"), format: perBlock },
          { label: "everyone eligible", values: pick("network_burn"), format: perBlock },
        ]),
      ];
    $(`[data-bot="${bot}"] .charts`).replaceChildren(...charts);
  }
}

function eventRow(event) {
  const row = element("tr", null, event.kind);
  row.append(
    element("td", time(event.at), "when"),
    element("td", event.height == null ? "" : event.height.toLocaleString("en-US"), "num"),
    element("td", event.bot || ""),
    element("td", event.kind),
    element("td", event.message),
  );
  if (event.data) row.title = JSON.stringify(event.data, null, 2);
  return row;
}

async function loadEvents(before) {
  const filter = $("#filter").value;
  const query = new URLSearchParams();
  if (before != null) query.set("before", before);
  if (filter) query.set("filter", filter);
  const events = await api(`/api/events?${query}`);
  const body = $("#events");
  if (before == null) body.replaceChildren();
  body.append(...events.map(eventRow));
  if (events.length) oldest = events[events.length - 1].id;
  $("#older").hidden = events.length < 100;
  return events;
}

// Only the first page refreshes itself; with older pages open it would jump.
function refreshEvents() {
  if ($("#events").children.length <= 100) loadEvents().catch(() => {});
}

async function refreshStatus() {
  try {
    renderStatus(await api("/api/status"));
  } catch (error) {
    $("#summary").textContent = `Control service unreachable: ${error.message}`;
  }
}

for (const form of document.querySelectorAll("form")) form.addEventListener("submit", save);
$("#range").addEventListener("change", refreshCharts);
$("#filter").addEventListener("change", () => loadEvents().catch(() => {}));
$("#older").addEventListener("click", () => loadEvents(oldest).catch(() => {}));

refreshStatus().then(refreshCharts);
loadEvents().catch(() => {});
setInterval(refreshStatus, 5000);
setInterval(refreshCharts, 60000);
setInterval(refreshEvents, 15000);
