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
    const next=(milliseconds=5000)=>deadline(queue.length?Promise.resolve(queue.shift()):new Promise<any>(done=>waiters.push(done)),milliseconds);
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
    // Two suspended reads with later frames queued behind them. Frame 5 arrives
    // after frame 3 starts, exercising reuse of a previous waiter's position.
    const send = (sequence:number,key:string) => socket!.send(JSON.stringify({operationId:`${epoch}:${sequence}`,key,payload:sequence}));
    const completion = async (sequence:number) => {
      expect(await next()).toEqual({key:"transport.ack",target:`${epoch}:${sequence}`});
      expect((await next()).payload).toEqual({ok:true,payload:sequence});
    };
    send(2,"read.wait");
    // Acceptance must arrive while the handler is still blocked, not after release.
    expect(await next()).toEqual({key:"transport.ack",target:`${epoch}:2`});
    await until(state=>state.readsStarted===1);
    send(3,"read.wait");
    send(4,"echo");
    await post("release");
    expect((await next()).payload).toEqual({ok:true,payload:2});
    await until(state=>state.readsStarted===2);
    expect(await next()).toEqual({key:"transport.ack",target:`${epoch}:3`});
    send(5,"echo");
    await post("release");
    expect((await next()).payload).toEqual({ok:true,payload:3});
    await completion(4);
    await completion(5);
    // Admission, unlike the earlier held handlers, has not emitted acceptance.
    // The observer deadline must remain bounded, and late admission must not
    // deliver an ack after its terminal timeout completion.
    send(6,"admission.wait");
    await until(state=>state.admissions===1);
    const timeout=await next(8000);
    expect(timeout.key).toBe("transport.complete");
    expect(timeout.target).toBe(`${epoch}:6`);
    expect(timeout.payload.error._tag).toBe("UnavailableError");
    await post("release");
    await until(state=>state.completed===4);
    send(7,"echo");
    await completion(7);
  } finally {socket?.close();await server.close();}
},120000);
}
