// Real Chromium regression for the narration view: one temporary API error must not freeze
// progress, and a finished narration with unresolved parts must show the fix-remaining banner.
// No backend, models or network downloads.
const {app,BrowserWindow}=require('electron');
const fs=require('fs'),path=require('path'),http=require('http'),os=require('os'),assert=require('assert');
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'voiceforge-recovery-test-'));
app.setPath('userData',temp);
app.on('window-all-closed',()=>{});
const dist=path.resolve(__dirname,'../dist');
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function verify(){
 let calls=0;
 const server=http.createServer((req,res)=>{
  const p=new URL(req.url,'http://local').pathname;
  if(req.method!=='GET'){res.writeHead(405);return res.end();}
  if(p==='/api/generations/test'){
   calls++;
   if(calls===2){res.writeHead(503);return res.end('{}');}  // one temporary failure
   const done=calls>=4;
   const gen={id:'test',profile_id:'test',profile_name:'Test',engine:'qwen3-tts-1.7b',language:'en',text:'Test clip.',title:'Recovery',
     status:done?'done':'running',message:done?'Done':'Working',progress:done?1:0.5,updated:1000,duration:4.2,
     file:done?'final.wav':undefined,job:done?null:{id:'mock',status:'running'},
     chunks:[{i:0,text:'Test clip.',file:'chunk_000.wav',status:done?'flagged':'running',issues:done?['cut_off']:[],attempts:[]}],
     final_scan:done?{checked:1,attempted:[0],fixed:[],remaining:[0]}:undefined,flagged:done?[0]:undefined};
   res.setHeader('Content-Type','application/json');return res.end(JSON.stringify(gen));
  }
  if(p.startsWith('/api/')){res.setHeader('Content-Type','application/json');return res.end(JSON.stringify(p==='/api/status'?{loaded_engine:null,running:null,queued:0,max_script_words:10000}:[]));}
  if(p==='/'){res.setHeader('Content-Type','text/html');return res.end(fs.readFileSync(path.join(dist,'index.html'),'utf8'));}
  const file=path.join(dist,p);res.setHeader('Content-Type',p.endsWith('.js')?'application/javascript':p.endsWith('.css')?'text/css':'application/octet-stream');
  if(fs.existsSync(file))return fs.createReadStream(file).pipe(res);res.writeHead(404);res.end();
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const win=new BrowserWindow({show:false,webPreferences:{backgroundThrottling:false}});
 try{
  await win.loadURL(`http://127.0.0.1:${server.address().port}/#/history/test`);
  let banner=false;
  for(let i=0;i<150;i++){
   banner=await win.webContents.executeJavaScript(`document.body.innerText.includes('Fix remaining errors')`);
   if(banner)break;
   await delay(100);
  }
  assert(calls>=4,`polling must continue past the temporary error (requests: ${calls})`);
  assert(banner,'the finished narration with unresolved parts must show the fix-remaining banner');
  console.log(JSON.stringify({requests:calls,banner}));
 }finally{win.destroy();server.closeAllConnections();server.close();}
}
(async()=>{await app.whenReady();await verify();console.log('Narration recovery regression passed');app.quit();})().catch(e=>{console.error(e);process.exitCode=1;app.exit(1);});
app.on('will-quit',()=>{try{fs.rmSync(temp,{recursive:true,force:true});}catch{}});
