// Downloads the Ollama sidecar into src-tauri/binaries/.
//
//   node setup-ollama.cjs           CPU only (smallest bundle)
//   node setup-ollama.cjs --gpu     also keep the Vulkan runner (any recent AMD/Intel/NVIDIA GPU;
//                                   Windows and Linux). Same as OLLAMA_GPU=1.
//
// Layout produced:
//   src-tauri/binaries/ollama-<target-triple>[.exe]   -> bundled via tauri.conf.json `externalBin`
//   src-tauri/binaries/lib/ollama/...                 -> bundled via `resources`
//
// Ollama looks for its model runner (llama-server + ggml libs) in a few places, among them
// <exe dir>/lib/ollama and <working dir>/build/lib/ollama:
//   - Windows: Tauri puts resources next to the exe, so lib/ollama/ just works.
//   - macOS/Linux: bundles keep resources away from the exe (Contents/Resources,
//     /usr/lib/<app>), so src-tauri/tauri.{macos,linux}.conf.json map the runner to
//     ollama-runtime/build/lib/ollama/, and ollama.rs starts the sidecar in ollama-runtime/.
//
// The release is pinned and checked against its sha256sum.txt. CUDA and ROCm folders are
// always skipped (they're gigabytes); models run on the CPU unless --gpu keeps Vulkan.
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { execSync } = require('child_process');

const VERSION = 'v0.32.4';
const RELEASE_URL = `https://github.com/ollama/ollama/releases/download/${VERSION}`;
const GPU = process.argv.includes('--gpu') || /^(1|true|yes)$/i.test(process.env.OLLAMA_GPU || '');

const platform = os.platform();
const arch = os.arch();

const TARGETS = {
    'win32-x64': { triple: 'x86_64-pc-windows-msvc', asset: 'ollama-windows-amd64.zip' },
    'win32-arm64': { triple: 'aarch64-pc-windows-msvc', asset: 'ollama-windows-arm64.zip' },
    'darwin-arm64': { triple: 'aarch64-apple-darwin', asset: 'ollama-darwin.tgz' },
    'darwin-x64': { triple: 'x86_64-apple-darwin', asset: 'ollama-darwin.tgz' },
    'linux-x64': { triple: 'x86_64-unknown-linux-gnu', asset: 'ollama-linux-amd64.tar.zst' },
    'linux-arm64': { triple: 'aarch64-unknown-linux-gnu', asset: 'ollama-linux-arm64.tar.zst' },
};
const target = TARGETS[`${platform}-${arch}`];
if (!target) {
    console.error(`Unsupported platform/architecture: ${platform} ${arch}`);
    process.exit(1);
}

const ext = platform === 'win32' ? '.exe' : '';
const binDir = path.join(__dirname, 'src-tauri', 'binaries');
const finalPath = path.join(binDir, `ollama-${target.triple}${ext}`);
const runnerDir = path.join(binDir, 'lib', 'ollama');
const versionFile = path.join(runnerDir, 'VERSION');
const vulkanDir = path.join(runnerDir, 'vulkan');

const skipDir = (name) => /^(cuda|rocm|mlx)/i.test(name) || (!GPU && /^vulkan/i.test(name));

function download(url, dest) {
    return new Promise((resolve, reject) => {
        const get = (u, redirects = 0) => {
            https
                .get(u, { headers: { 'User-Agent': 'diy-waifu-setup' } }, (res) => {
                    if ([301, 302, 303, 307, 308].includes(res.statusCode) && res.headers.location && redirects < 5) {
                        res.resume();
                        return get(res.headers.location, redirects + 1);
                    }
                    if (res.statusCode !== 200) {
                        res.resume();
                        return reject(new Error(`HTTP ${res.statusCode} for ${u}`));
                    }
                    const total = Number(res.headers['content-length']) || 0;
                    let got = 0;
                    let shown = -1;
                    res.on('data', (chunk) => {
                        got += chunk.length;
                        const pct = total ? Math.floor((got / total) * 100) : -1;
                        if (pct !== shown && pct % 10 === 0) {
                            shown = pct;
                            process.stdout.write(`   ${pct}%\r`);
                        }
                    });
                    const file = fs.createWriteStream(dest);
                    res.pipe(file);
                    file.on('finish', () => file.close(resolve));
                    file.on('error', reject);
                })
                .on('error', reject);
        };
        get(url);
    });
}

function sha256(file) {
    return new Promise((resolve, reject) => {
        const h = crypto.createHash('sha256');
        fs.createReadStream(file).on('data', (d) => h.update(d)).on('end', () => resolve(h.digest('hex'))).on('error', reject);
    });
}

function extract(archive, dest) {
    fs.mkdirSync(dest, { recursive: true });
    if (archive.endsWith('.zip')) {
        // PowerShell directly, because Git Bash's `tar` reads C: as a remote host
        execSync(
            `powershell -NoProfile -Command "Expand-Archive -LiteralPath '${archive}' -DestinationPath '${dest}' -Force"`,
            { stdio: 'inherit' }
        );
    } else if (archive.endsWith('.tar.zst')) {
        try {
            execSync(`tar --zstd -xf "${archive}" -C "${dest}"`, { stdio: 'inherit' });
        } catch {
            execSync(`zstd -dc "${archive}" | tar -xf - -C "${dest}"`, { stdio: 'inherit', shell: '/bin/sh' });
        }
    } else {
        execSync(`tar -xzf "${archive}" -C "${dest}"`, { stdio: 'inherit' });
    }
}

// Copies a runner folder, leaving out the GPU backends we don't ship
function copyRunner(srcDir, destDir, exclude = new Set()) {
    fs.mkdirSync(destDir, { recursive: true });
    for (const entry of fs.readdirSync(srcDir, { withFileTypes: true })) {
        if (exclude.has(entry.name) || (entry.isDirectory() && skipDir(entry.name))) continue;
        fs.cpSync(path.join(srcDir, entry.name), path.join(destDir, entry.name), { recursive: true });
    }
}

function findFile(dir, name) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const p = path.join(dir, entry.name);
        if (entry.isFile() && entry.name === name) return p;
        if (entry.isDirectory()) {
            const found = findFile(p, name);
            if (found) return found;
        }
    }
    return null;
}

(async () => {
    try {
        fs.mkdirSync(runnerDir, { recursive: true }); // `resources` needs it to exist

        const current =
            fs.existsSync(finalPath) && fs.existsSync(versionFile) && fs.readFileSync(versionFile, 'utf8').trim() === VERSION;
        // Older setups had no VERSION file: accept a working runner rather than re-download 1.6 GB
        const legacyOk = !fs.existsSync(versionFile) && fs.existsSync(finalPath) && fs.existsSync(path.join(runnerDir, `llama-server${ext}`));
        const gpuOk = !GPU || platform === 'darwin' || fs.existsSync(vulkanDir);
        if ((current || legacyOk) && gpuOk) {
            console.log(`✅ Ollama sidecar ready for ${target.triple}${fs.existsSync(vulkanDir) ? ' (with Vulkan GPU runner)' : ''}`);
            return;
        }

        const workDir = path.join(binDir, '.download');
        const archive = path.join(workDir, target.asset);
        const extractDir = path.join(workDir, 'extract');
        fs.rmSync(workDir, { recursive: true, force: true });
        fs.mkdirSync(workDir, { recursive: true });

        console.log(`📥 Downloading Ollama ${VERSION} (${target.asset}). This is large; it's done once.`);
        await download(`${RELEASE_URL}/${target.asset}`, archive);
        await download(`${RELEASE_URL}/sha256sum.txt`, path.join(workDir, 'sha256sum.txt'));
        const sums = fs.readFileSync(path.join(workDir, 'sha256sum.txt'), 'utf8');
        const line = sums.split('\n').find((l) => l.trim().endsWith(target.asset));
        const expected = line && line.trim().split(/\s+/)[0];
        const actual = await sha256(archive);
        if (!expected || expected.toLowerCase() !== actual) {
            throw new Error(`Checksum mismatch for ${target.asset} (expected ${expected}, got ${actual})`);
        }

        console.log('📦 Extracting...');
        extract(archive, extractDir);

        const exe = findFile(extractDir, `ollama${ext}`);
        if (!exe) throw new Error('ollama binary not found in the archive');
        // Runner: lib/ollama on Windows/Linux; on macOS the helpers sit next to the binary
        const libDir = path.join(extractDir, 'lib', 'ollama');
        fs.rmSync(runnerDir, { recursive: true, force: true });
        if (fs.existsSync(libDir)) {
            copyRunner(libDir, runnerDir);
        } else {
            copyRunner(path.dirname(exe), runnerDir, new Set([path.basename(exe)]));
        }
        if (GPU && platform !== 'darwin' && !fs.existsSync(vulkanDir)) {
            console.warn('⚠️  This Ollama build has no Vulkan runner; models will run on the CPU.');
        }
        fs.copyFileSync(exe, finalPath);
        if (platform !== 'win32') fs.chmodSync(finalPath, 0o755);
        fs.writeFileSync(versionFile, VERSION + '\n');
        fs.rmSync(workDir, { recursive: true, force: true });

        console.log(`🎉 Ollama ${VERSION} ready for ${target.triple}${fs.existsSync(vulkanDir) ? ' with the Vulkan GPU runner' : ' (CPU)'}`);
    } catch (err) {
        console.error('❌ Failed to set up the Ollama sidecar:', err.message || err);
        process.exit(1);
    }
})();
