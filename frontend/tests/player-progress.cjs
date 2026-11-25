// Real Chromium media/React lifecycle regression; no backend, models or network downloads.
const {app,BrowserWindow}=require('electron');
const fs=require('fs'),path=require('path'),http=require('http'),os=require('os'),assert=require('assert');
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'voiceforge-player-test-'));
app.setPath('userData',temp);app.commandLine.appendSwitch('autoplay-policy','no-user-gesture-required');
app.on('window-all-closed',()=>{});
const dist=path.resolve(__dirname,'../dist');
const sr=24000,n=sr*10,wav=Buffer.alloc(44+n*2);
wav.write('RIFF');wav.writeUInt32LE(36+n*2,4);wav.write('WAVEfmt ',8);wav.writeUInt32LE(16,16);wav.writeUInt16LE(1,20);wav.writeUInt16LE(1,22);wav.writeUInt32LE(sr,24);wav.writeUInt32LE(sr*2,28);wav.writeUInt16LE(2,32);wav.writeUInt16LE(16,34);wav.write('data',36);wav.writeUInt32LE(n*2,40);
for(let i=0;i<n;i++)wav.writeInt16LE(Math.round(Math.sin(i*2*Math.PI*220/sr)*1000),44+i*2);
const injection=`<script>window.__audios=[];let create=document.createElement.bind(document);document.createElement=function(t,...a){let e=create(t,...a);if(t==='audio')window.__audios.push(e);return e;};</script>`;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function verify(legacy){
 let revision=legacy?undefined:1,requests=0;
 const server=http.createServer((req,res)=>{
  const p=new URL(req.url,'http://local').pathname;
  if(req.method!=='GET'){res.writeHead(405);return res.end();}
  if(p==='/api/generations/test'){
   const gen={id:'test',profile_id:'test',profile_name:'Test',engine:'qwen3-tts-1.7b',language:'en',text:'Test clip.',title:'Progress '+requests,status:'running',updated:++requests,job:{id:'mock',status:'running'},chunks:[{i:0,text:'Test clip.',file:'chunk_000.wav',status:'ok',audio_revision:revision,attempts:[{seed:1234,secs:1}]}]};
   res.setHeader('Content-Type','application/json');return res.end(JSON.stringify(gen));
  }
  if(p.endsWith('/audio')){res.setHeader('Content-Type','audio/wav');return res.end(wav);}
  if(p.startsWith('/api/')){res.setHeader('Content-Type','application/json');return res.end(JSON.stringify(p==='/api/status'?{loaded_engine:null,running:null,queued:0,max_script_words:10000}:[]));}
  if(p==='/'){res.setHeader('Content-Type','text/html');return res.end(fs.readFileSync(path.join(dist,'index.html'),'utf8').replace('<head>','<head>'+injection));}
  const file=path.join(dist,p);res.setHeader('Content-Type',p.endsWith('.js')?'application/javascript':p.endsWith('.css')?'text/css':'application/octet-stream');
  if(fs.existsSync(file))return fs.createReadStream(file).pipe(res);res.writeHead(404);res.end();
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const win=new BrowserWindow({show:false,webPreferences:{backgroundThrottling:false}});win.webContents.setAudioMuted(true);
 try{
  await win.loadURL(`http://127.0.0.1:${server.address().port}/#/history/test`);
  for(let i=0;i<50;i++){if(await win.webContents.executeJavaScript(`!!document.querySelector('button[title="Listen"]')`))break;await delay(100);}
  await win.webContents.executeJavaScript(`document.querySelector('button[title="Listen"]').click()`);await delay(300);
  await win.webContents.executeJavaScript(`document.querySelector('button[aria-label="Play"]').click()`);await delay(3500);
  const playing=await win.webContents.executeJavaScript(`({count:__audios.length,time:__audios[0].currentTime,paused:__audios[0].paused})`);
  assert.equal(playing.count,1,'Other parts/progress must not recreate the clip player');assert(!playing.paused,'Playback must continue through progress updates');assert(playing.time>2,'Playback must advance');assert(requests>=3);
  revision=2;await delay(1200);
  const replaced=await win.webContents.executeJavaScript(`({count:__audios.length,oldPaused:__audios[0].paused,newPaused:__audios[1]?.paused})`);
  assert.equal(replaced.count,2,'Replacing this clip must reload its new audio');assert(replaced.oldPaused&&replaced.newPaused,'Replacement must stop old audio without autoplay');
  console.log(JSON.stringify({legacy,requests,playing,replaced}));
 }finally{win.destroy();server.closeAllConnections();server.close();}
}
(async()=>{await app.whenReady();await verify(process.argv.includes('legacy'));console.log('Playback progress regression passed');app.quit();})().catch(e=>{console.error(e);process.exitCode=1;app.exit(1);});
app.on('will-quit',()=>{try{fs.rmSync(temp,{recursive:true,force:true});}catch{}});
