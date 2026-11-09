// Downloads the whisper.cpp speech-to-text server into src-tauri/binaries/whisper/.
//
// Layout produced (bundled via tauri.conf.json `resources` -> <app dir>/whisper/):
//   src-tauri/binaries/whisper/whisper-server.exe
//   src-tauri/binaries/whisper/whisper.dll, ggml*.dll   (CPU backends; the best one is picked at runtime)
//
// The release is pinned and its SHA-256 checked, since this is code the app runs.
// The speech model itself (~150 MB) is downloaded later from the app's Voice tab.
// Only Windows x64 is handled for now; elsewhere this leaves a placeholder so the
// `resources` entry still exists.
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { execSync } = require('child_process');

const RELEASE = 'b5454';
const URL = `https://github.com/ggml-org/whisper.cpp/releases/download/${RELEASE}/whisper-bin-x64.zip`;
const SHA256 = '6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb';

const outDir = path.join(__dirname, 'src-tauri', 'binaries', 'whisper');
const serverExe = path.join(outDir, 'whisper-server.exe');
const versionFile = path.join(outDir, 'VERSION');

// whisper-server and the libraries it loads; everything else in the zip is other tools
const KEEP = (name) => name === 'whisper-server.exe' || name === 'whisper.dll' || /^ggml.*\.dll$/i.test(name);

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

async function main() {
    if (!(os.platform() === 'win32' && os.arch() === 'x64')) {
        fs.mkdirSync(outDir, { recursive: true });
        fs.writeFileSync(path.join(outDir, 'README.txt'), 'Speech-to-text is only set up on Windows x64 for now.\n');
        console.log('Whisper: skipped (only Windows x64 is supported for now).');
        return;
    }
    if (fs.existsSync(serverExe) && fs.existsSync(versionFile) && fs.readFileSync(versionFile, 'utf8').trim() === RELEASE) {
        console.log(`Whisper ${RELEASE} already present.`);
        return;
    }

    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'whisper-'));
    const zip = path.join(tmp, 'whisper.zip');
    console.log(`Downloading whisper.cpp ${RELEASE}...`);
    await download(URL, zip);

    const hash = crypto.createHash('sha256').update(fs.readFileSync(zip)).digest('hex');
    if (hash !== SHA256) {
        throw new Error(`Checksum mismatch for whisper.cpp download (got ${hash}). Not installing it.`);
    }

    // Windows' own bsdtar (10+) reads zip files. Use its full path: a Git Bash
    // `tar` earlier on PATH would read "C:" as a remote host.
    const extractDir = path.join(tmp, 'x');
    fs.mkdirSync(extractDir);
    const tar = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'tar.exe');
    execSync(`"${tar}" -xf "${zip}" -C "${extractDir}"`, { stdio: 'inherit' });
    const releaseDir = path.join(extractDir, 'Release');

    fs.rmSync(outDir, { recursive: true, force: true });
    fs.mkdirSync(outDir, { recursive: true });
    let copied = 0;
    for (const name of fs.readdirSync(releaseDir)) {
        if (KEEP(name)) {
            fs.copyFileSync(path.join(releaseDir, name), path.join(outDir, name));
            copied++;
        }
    }
    if (!fs.existsSync(serverExe)) throw new Error('whisper-server.exe was not in the download');
    fs.writeFileSync(versionFile, RELEASE + '\n');
    fs.rmSync(tmp, { recursive: true, force: true });
    console.log(`Whisper ${RELEASE} installed (${copied} files) in ${outDir}`);
}

main().catch((e) => {
    console.error(`Whisper setup failed: ${e.message}`);
    process.exit(1);
});
