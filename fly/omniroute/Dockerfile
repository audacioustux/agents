FROM node:22-bookworm-slim

ENV NODE_ENV=production \
    HOSTNAME=0.0.0.0 \
    PORT=20128 \
    DATA_DIR=/data

WORKDIR /app

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates python3 make g++; \
    rm -rf /var/lib/apt/lists/*; \
    version="$(npm view omniroute time --json | node -e 'const fs=require("node:fs"); const time=JSON.parse(fs.readFileSync(0,"utf8")); const cutoff=Date.now()-3*24*60*60*1000; const cmp=(a,b)=>{const A=a.split(".").map(Number),B=b.split(".").map(Number); return A[0]-B[0]||A[1]-B[1]||A[2]-B[2];}; const versions=Object.keys(time).filter(v=>/^\d+\.\d+\.\d+$/.test(v)&&Date.parse(time[v])<=cutoff).sort(cmp); const version=versions.at(-1); if(!version){throw new Error("No stable omniroute release is at least 3 days old");} process.stdout.write(version);')"; \
    npm install -g "omniroute@${version}"; \
    npm cache clean --force; \
    mkdir -p /data

EXPOSE 20128

CMD ["omniroute", "serve", "--no-open", "--log"]
