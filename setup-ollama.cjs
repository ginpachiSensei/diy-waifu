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

// 2. Ensure the binaries directory exists
if (!fs.existsSync(binDir)) {
    fs.mkdirSync(binDir, { recursive: true });
}

// 3. Skip if the file is already downloaded
if (fs.existsSync(finalPath)) {
    console.log(`✅ Ollama sidecar already exists for ${targetTriple}`);
    process.exit(0);
}

console.log(`📥 Downloading Ollama for ${targetTriple}...`);

// Helper to handle redirects during download
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
            file.on('finish', () => {
                file.close(resolve);
            });
        }).on('error', (err) => {
            fs.unlink(dest, () => reject(err));
        });
    });
}

// 4. Download and process the file
(async () => {
    try {
        if (isZip) {
            const zipPath = path.join(__dirname + "/src-tauri/binaries", 'ollama.zip');
            await downloadFile(url, zipPath);
            
            console.log("📦 Extracting zip archive...");
            // Use PowerShell directly to avoid Git Bash `tar` conflicts
            execSync(`powershell -Command "Expand-Archive -Path ollama.zip -DestinationPath . -Force"`, { cwd: __dirname });
            
            console.log("🚚 Moving binary to src-tauri/binaries...");
            fs.renameSync(path.join(__dirname, 'ollama.exe'), finalPath);
            fs.unlinkSync(zipPath); // Cleanup
        } else {
            await downloadFile(url, finalPath);
            // On Unix systems, we must make the binary executable
            fs.chmodSync(finalPath, 0o755);
        }
        
        console.log(`🎉 Successfully prepared Ollama sidecar: ${binaryName}`);
    } catch (err) {
        console.error("❌ Failed to download Ollama sidecar:", err);
        process.exit(1);
    }
})();
