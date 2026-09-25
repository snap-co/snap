import { test, expect } from "bun:test";
import { createHmac } from "node:crypto";
import { resolve } from "node:path";
import { deadline, startServer } from "../adapters/server";
import { workersServer } from "../adapters/workers";

for (const host of ["native", "workers"] as const) {
test(`${host}: one operation is bound to HTTP and WebSocket; an accepted continuation outlives its observer`, async () => {
  const server=await (host === "native" ? startServer({executable:resolve(import.meta.dirname,"../../target/debug/examples/carrier-contract")}) : workersServer("contract"));
  const post=async (key:string,payload:unknown=null,signal?:AbortSignal)=> {
    const response=await fetch(`${server.baseUrl}/${key}`,{method:"POST",headers:{"x-snap-build":"healthy-smoke","x-snap-operation-id":"same-caller-id","content-type":"application/json"},body:JSON.stringify(payload),signal});
    return (await response.json()).payload.payload;
  };
  let socket:WebSocket|undefined;
  try {
    const payload={nested:{value:42}};
    expect(await post("echo",payload)).toEqual(payload);
    const signature=createHmac("sha256",Buffer.alloc(32,7)).update("fixture").digest("base64url");
    socket=new WebSocket(`${server.baseUrl.replace("http:","ws:")}/_transport/ws?build=healthy-smoke&clientId=fixture`,{headers:{origin:server.baseUrl,cookie:`fixture_session=fixture.${signature}`}});
    const queue:any[]=[];const waiters:((value:any)=>void)[]=[];
    socket.onmessage=event=> {const value=JSON.parse(String(event.data));const waiter=waiters.shift();if(waiter)waiter(value);else queue.push(value);};
    const next=()=>deadline(queue.length?Promise.resolve(queue.shift()):new Promise<any>(done=>waiters.push(done)),5000);
    const epoch=(await next()).payload.epoch;
    socket.send(JSON.stringify({operationId:`${epoch}:1`,key:"echo",payload}));
    expect((await next()).key).toBe("transport.ack");
    expect((await next()).payload.payload).toEqual(payload);
    const abort=new AbortController();
    const held=post("hold",null,abort.signal).catch(()=>undefined);
    const until=async (condition:(state:any)=>boolean)=> {const end=Date.now()+5000;while(Date.now()<end) {const state=await post("status");if(condition(state))return state;await Bun.sleep(10);}throw new Error("Execution condition was not reached");};
    await until(state=>state.started);
    abort.abort();await held;
    expect(await post("echo",payload)).toEqual(payload);
    await post("release");
    expect((await until(state=>state.completed===1)).completed).toBe(1);
  } finally {socket?.close();await server.close();}
},120000);
}
