// Downloads the Ollama sidecar into src-tauri/binaries/.
//
// Layout produced:
//   src-tauri/binaries/ollama-<target-triple>[.exe]   -> bundled via tauri.conf.json `externalBin`
//   src-tauri/binaries/lib/ollama/...                 -> bundled via `resources` to <app dir>/lib/ollama
//
// Ollama looks for its model runner (llama-server + ggml libs) in lib/ollama next to its own
// executable, so both pieces must ship together. GPU runner folders (CUDA/ROCm/Vulkan) are
// skipped to keep the bundle small, so models run on CPU.
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');
const { execSync } = require('child_process');

const platform = os.platform();
const arch = os.arch();

let targetTriple = '';
let url = '';
let isZip = false;

// 1. Detect OS & Architecture to map to Tauri's target triple
if (platform === 'win32' && arch === 'x64') {
    targetTriple = 'x86_64-pc-windows-msvc';
    url = 'https://github.com/ollama/ollama/releases/latest/download/ollama-windows-amd64.zip';
    isZip = true;
} else if (platform === 'darwin' && arch === 'arm64') {
    targetTriple = 'aarch64-apple-darwin';
    url = 'https://github.com/ollama/ollama/releases/latest/download/ollama-darwin';
} else if (platform === 'darwin' && arch === 'x64') {
    targetTriple = 'x86_64-apple-darwin';
    url = 'https://github.com/ollama/ollama/releases/latest/download/ollama-darwin';
} else if (platform === 'linux' && arch === 'x64') {
    targetTriple = 'x86_64-unknown-linux-gnu';
    url = 'https://github.com/ollama/ollama/releases/latest/download/ollama-linux-amd64';
} else if (platform === 'linux' && arch === 'arm64') {
    targetTriple = 'aarch64-unknown-linux-gnu';
    url = 'https://github.com/ollama/ollama/releases/latest/download/ollama-linux-arm64';
} else {
    console.error(`Unsupported platform/architecture: ${platform} ${arch}`);
    process.exit(1);
}

const ext = platform === 'win32' ? '.exe' : '';
const binaryName = `ollama-${targetTriple}${ext}`;
const binDir = path.join(__dirname, 'src-tauri', 'binaries');
const finalPath = path.join(binDir, binaryName);
const runnerDir = path.join(binDir, 'lib', 'ollama');
const runnerExe = path.join(runnerDir, `llama-server${ext}`);
// Older versions of this script extracted the zip into the repo root and left lib/ollama there.
const legacyRunnerDir = path.join(__dirname, 'lib', 'ollama');

const isGpuDir = (name) => /^(cuda|rocm|vulkan)/i.test(name);

// Copy the runner folder, leaving out the GPU backends
function copyCpuRunner(srcDir, destDir) {
    fs.mkdirSync(destDir, { recursive: true });
    for (const entry of fs.readdirSync(srcDir, { withFileTypes: true })) {
        if (entry.isDirectory() && isGpuDir(entry.name)) continue;
        fs.cpSync(path.join(srcDir, entry.name), path.join(destDir, entry.name), { recursive: true });
    }
}

function downloadFile(url, dest) {
    return new Promise((resolve, reject) => {
        https.get(url, (response) => {
            if (response.statusCode === 301 || response.statusCode === 302) {
                return resolve(downloadFile(response.headers.location, dest));
            }
            if (response.statusCode !== 200) {
                return reject(new Error(`Failed to download: ${response.statusCode}`));
            }
            const file = fs.createWriteStream(dest);
            response.pipe(file);
            file.on('finish', () => file.close(resolve));
        }).on('error', (err) => {
            fs.unlink(dest, () => reject(err));
        });
    });
}

(async () => {
    try {
        fs.mkdirSync(binDir, { recursive: true });
        // Tauri's `resources` entry needs this folder to exist, even on platforms without a runner
        fs.mkdirSync(runnerDir, { recursive: true });

        const haveBinary = fs.existsSync(finalPath);
        const haveRunner = !isZip || fs.existsSync(runnerExe);

        if (haveBinary && haveRunner) {
            console.log(`✅ Ollama sidecar already exists for ${targetTriple}`);
            return;
        }

        // Reuse a leftover runner from an older setup instead of re-downloading
        if (haveBinary && !haveRunner && fs.existsSync(path.join(legacyRunnerDir, `llama-server${ext}`))) {
            console.log('🚚 Reusing runner from ./lib/ollama (CPU files only)...');
            copyCpuRunner(legacyRunnerDir, runnerDir);
            console.log('🎉 Ollama runner ready. You can delete ./lib to free disk space.');
            return;
        }

        console.log(`📥 Downloading Ollama for ${targetTriple}...`);

        if (isZip) {
            const workDir = path.join(binDir, '.download');
            const zipPath = path.join(workDir, 'ollama.zip');
            const extractDir = path.join(workDir, 'extract');
            fs.rmSync(workDir, { recursive: true, force: true });
            fs.mkdirSync(workDir, { recursive: true });

            await downloadFile(url, zipPath);

            console.log('📦 Extracting zip archive...');
            // Use PowerShell directly to avoid Git Bash `tar` conflicts
            execSync(
                `powershell -NoProfile -Command "Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${extractDir}' -Force"`,
                { stdio: 'inherit' }
            );

            console.log('🚚 Moving binary and CPU runner into src-tauri/binaries...');
            fs.copyFileSync(path.join(extractDir, `ollama${ext}`), finalPath);
            copyCpuRunner(path.join(extractDir, 'lib', 'ollama'), runnerDir);
            fs.rmSync(workDir, { recursive: true, force: true });
        } else {
            await downloadFile(url, finalPath);
            // On Unix systems, we must make the binary executable
            fs.chmodSync(finalPath, 0o755);
        }

        console.log(`🎉 Successfully prepared Ollama sidecar: ${binaryName}`);
    } catch (err) {
        console.error('❌ Failed to download Ollama sidecar:', err);
        process.exit(1);
    }
})();
