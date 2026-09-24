import { test, expect } from "bun:test";
import { authyServer } from "../adapters/authy";
import { legacyPassport } from "../adapters/legacy-passport";

test("original Authy accounts, password hashes, sessions and signing keys survive Store migration", async ()=>{
  let legacy!: Awaited<ReturnType<typeof legacyPassport>>;
  const server=await authyServer(false,async database=>{legacy=await legacyPassport(database);});
  const request=async(key:string,payload?:unknown,cookie?:string)=>{
    const response=await fetch(`${server.baseUrl}/${key.replaceAll(".","/")}`,{method:payload===undefined?"GET":"POST",headers:{"x-snap-build":"healthy-smoke","x-snap-operation-id":"migration","content-type":"application/json",...(cookie?{cookie}:{})},body:payload===undefined?undefined:JSON.stringify(payload)});
    return {response, event:await response.json()};
  };
  try {
    expect((await request("identity.fetch",undefined,legacy.cookie)).event.payload.payload.identityId).toBe(legacy.identity);
    const signed=await request("identity.password.acquire",{kind:"user",email:"legacy@example.test",password:"original password"});
    expect(signed.event.payload.payload).toEqual({_tag:"Approved"});
    const newCookie=signed.response.headers.get("set-cookie")!.split(";")[0];
    expect((await request("identity.fetch",undefined,newCookie)).event.payload.payload.identityId).toBe(legacy.identity);
    await server.restart();
    expect((await request("identity.fetch",undefined,legacy.cookie)).event.payload.payload.identityId).toBe(legacy.identity);
    expect((await request("identity.fetch",undefined,newCookie)).event.payload.payload.identityId).toBe(legacy.identity);
    expect((await request("account.create",{email:"legacy@example.test",password:"different password"})).event.payload.error.failure._tag).toBe("EnrollFailedError");
  } finally {await server.close();}
},20000);
