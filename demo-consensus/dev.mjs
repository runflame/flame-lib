import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";

const demoDirectory = dirname(fileURLToPath(import.meta.url));
const repository = dirname(demoDirectory);
const frontend = join(demoDirectory, "frontend");
const children = new Set();
let vite;
let stopping = false;

function start(
  command,
  args,
  { cwd = repository, capture = false, service = false } = {},
) {
  const child = spawn(command, args, {
    cwd,
    detached: true,
    stdio: ["ignore", capture ? "pipe" : "inherit", "inherit"],
  });
  const entry = { child, service };
  entry.done = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal }));
  }).finally(() => children.delete(entry));
  children.add(entry);
  return entry;
}

function signal(entry, force = false) {
  if (!entry.child.pid) return;
  try {
    process.kill(
      entry.service && !force ? entry.child.pid : -entry.child.pid,
      force ? "SIGKILL" : "SIGTERM",
    );
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

async function stop(code) {
  if (stopping) return;
  stopping = true;
  process.exitCode = code;
  console.log("\nStopping frontend and backend…");
  const running = [...children];
  for (const entry of running) signal(entry);
  const deadline = setTimeout(() => {
    for (const entry of children) signal(entry, true);
  }, 25_000);
  try {
    await Promise.allSettled([
      vite?.close(),
      ...running.map((entry) => entry.done),
    ]);
  } finally {
    clearTimeout(deadline);
  }
}

async function buildBackend() {
  console.log("Building backend…");
  const build = start(
    "cargo",
    [
      "build",
      "-p",
      "demo-consensus-backend",
      "--message-format=json-render-diagnostics",
    ],
    { capture: true },
  );
  let executable;
  const readArtifacts = async () => {
    for await (const line of createInterface({ input: build.child.stdout })) {
      const message = JSON.parse(line);
      if (
        message.reason === "compiler-artifact" &&
        message.target.name === "demo-consensus-backend" &&
        message.executable
      ) {
        executable = message.executable;
      }
    }
  };
  const [result] = await Promise.all([build.done, readArtifacts()]);
  if (stopping) return;
  if (result.code !== 0 || !executable)
    throw new Error("Backend build failed.");
  return executable;
}

async function main() {
  if (!existsSync(join(frontend, "node_modules", "vite"))) {
    console.log("Installing frontend dependencies…");
    const install = await start("npm", ["ci"], { cwd: frontend }).done;
    if (stopping) return;
    if (install.code !== 0)
      throw new Error("Frontend dependency installation failed.");
  }
  const executable = await buildBackend();
  if (stopping) return;
  const require = createRequire(join(frontend, "package.json"));
  const { createServer } = await import(
    pathToFileURL(require.resolve("vite")).href
  );
  if (stopping) return;
  vite = await createServer({ root: frontend, server: { host: "127.0.0.1" } });
  if (stopping) {
    await vite.close();
    return;
  }
  await vite.listen();
  if (stopping) {
    await vite.close();
    return;
  }
  vite.httpServer.once("close", () => {
    if (!stopping) void stop(1);
  });
  vite.httpServer.on("error", (error) => {
    console.error(error.message);
    void stop(1);
  });
  vite.printUrls();
  const backend = start(executable, [], { service: true });
  console.log("Press Ctrl+C to stop both services.");
  const result = await backend.done;
  if (!stopping) {
    console.error(`Backend stopped (${result.signal ?? result.code}).`);
    signal(backend, true);
    await stop(result.code || 1);
  }
}

process.on("SIGINT", () => void stop(0));
process.on("SIGTERM", () => void stop(0));

main().catch(async (error) => {
  if (!stopping) {
    console.error(error.message);
    await stop(1);
  }
});
