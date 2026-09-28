import { expect, test } from "bun:test";
import { Factorio } from "../client";

test("browser reconnect refreshes its lease and recovers the same accepted invocation", async () => {
  let identityRequests=0, attachments=0, effects=0, leaseFresh=false, acceptedID=0;
  const server=Bun.serve({hostname:"127.0.0.1",port:0,
    fetch(request,server) {
      if(new URL(request.url).pathname==="/transport" && server.upgrade(request)) return;
      identityRequests++;
      if(identityRequests===2) return new Response("restarting",{status:503});
      leaseFresh=true;
      return Response.json({identified:true,owner:"owner",csrf:`csrf-${identityRequests}`});
    }, websocket:{message(socket,message) {
      const frame=JSON.parse(String(message));
      if(frame.Connect) {
        attachments++;
        if(!leaseFresh) { socket.send(JSON.stringify({Failed:"InvalidBearer"}));return; }
        socket.send(JSON.stringify({Attached:{resumed:attachments>1}}));return;
      }
      const call=frame.Invoke;
      if(!acceptedID) {
        acceptedID=call.id;effects++;
        socket.send(JSON.stringify({Events:[{Accepted:{id:call.id}}]}));
        leaseFresh=false;socket.close();return;
      }
      expect(call.id).toBe(acceptedID);
      socket.send(JSON.stringify({Events:[{Completed:{id:call.id,outcome:{Ok:"recovered"}}}]}));
    }}
  });
  const client=new Factorio(`http://127.0.0.1:${server.port}`);
  try {
    expect(await client.invoke("test.change",{})).toBe("recovered");
    expect(identityRequests).toBe(3);expect(attachments).toBe(2);expect(effects).toBe(1);
    expect(client.identity.csrf).toBe("csrf-3");
  } finally {client.close();server.stop(true);}
});

test("terminal reconnect rejection settles an accepted agent command without replay",async()=>{
  let attachments=0, effects=0;
  const server=Bun.serve({hostname:"127.0.0.1",port:0,
    fetch(request,server) {
      if(new URL(request.url).pathname==="/transport" && server.upgrade(request)) return;
      return Response.json({identified:true,owner:"owner",human:false});
    },websocket:{message(socket,message){
      const frame=JSON.parse(String(message));
      if(frame.Connect){attachments++;socket.send(JSON.stringify(attachments===1?{Attached:{resumed:false}}:{Failed:"InvalidBearer"}));return;}
      effects++;
      socket.send(JSON.stringify({Events:[{Accepted:{id:frame.Invoke.id}}]}));socket.close();
    }}
  });
  const client=new Factorio(`http://127.0.0.1:${server.port}`,"fixture-agent-token");
  try {
    await expect(client.invoke("test.change",{})).rejects.toThrow("InvalidBearer");
    expect(attachments).toBe(2);expect(effects).toBe(1);
  } finally {client.close();server.stop(true);}
});

test("an ended browser login settles outstanding work during reconnect bootstrap",async()=>{
  let identityRequests=0;
  const server=Bun.serve({hostname:"127.0.0.1",port:0,
    fetch(request,server){
      if(new URL(request.url).pathname==="/transport" && server.upgrade(request)) return;
      return Response.json(++identityRequests===1?{identified:true,owner:"owner"}:{identified:false});
    },websocket:{message(socket,message){
      const frame=JSON.parse(String(message));
      if(frame.Connect){socket.send(JSON.stringify({Attached:{resumed:false}}));return;}
      socket.send(JSON.stringify({Events:[{Accepted:{id:frame.Invoke.id}}]}));socket.close();
    }}
  });
  const client=new Factorio(`http://127.0.0.1:${server.port}`);
  try {await expect(client.invoke("test.change",{})).rejects.toThrow("outstanding outcomes are unknown");expect(identityRequests).toBe(2);}
  finally {client.close();server.stop(true);}
});
