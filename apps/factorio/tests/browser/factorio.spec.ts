import { test, expect } from "@playwright/test";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { writeFile, readFile } from "node:fs/promises";
import { resolve } from "node:path";
const exec = promisify(execFile);
const base=process.env.FACTORIO_TEST_URL!, dir=process.env.FACTORIO_FIXTURE_DIR!, root=resolve(import.meta.dirname,"../../../..");
test("fixture-only human acceptance, CLI/UI records, exclusions and restart recovery",async({page})=>{
  const errors:string[]=[];page.on("pageerror",e=>errors.push(e.message));
  await page.goto(base);await page.getByRole("link",{name:"Continue with Authy"}).click();
  await page.getByRole("button",{name:"New here? Create account",exact:true}).click();
  await page.getByLabel("Email",{exact:true}).fill(`factorio-${Date.now()}@example.test`);await page.getByLabel("Password",{exact:true}).fill("Factorio fixture password");
  await page.getByRole("button",{name:"Create account",exact:true}).click();await page.getByRole("button",{name:"Allow",exact:true}).click();
  await expect(page.getByRole("heading",{name:"Tickets",exact:true})).toBeVisible();
  await page.getByRole("button",{name:"Create agent token",exact:true}).click();const token=await page.getByLabel("Agent token",{exact:true}).inputValue();expect(token.length).toBeGreaterThan(32);await page.getByRole("button",{name:"Dismiss token"}).click();
  async function cli(args:string[]){const {stdout}=await exec("bun",[`${root}/apps/factorio/cli.ts`,...args],{cwd:root,env:{...process.env,FACTORIO_ORIGIN:base,FACTORIO_TOKEN:token}});return JSON.parse(stdout);}
  const ticket=(id:string,blockers:string[]=[])=>({id,title:id,description:"Fixture implementation",modules:["a"],status:"ready",notes:"",parent:null,blockers});
  for(const t of [ticket("first"),ticket("dependent",["first"])]){const file=`${dir}/${t.id}.json`;await writeFile(file,JSON.stringify(t));await cli(["ticket",file]);}
  await expect(page.getByRole("heading",{name:"first: first",exact:true})).toBeVisible();await expect(page.getByRole("link",{name:"first (ready)"})).toBeVisible();
  await expect(cli(["start","--id","blocked","--tickets","dependent","--modules","a","--","blocked"])).rejects.toThrow();
  const a=(await cli(["start","--id","one","--tickets","first","--modules","a","--","first change"])).session;
  const b=(await cli(["start","--id","two","--modules","b","--","parallel change"])).session;
  expect(a.port).not.toBe(b.port);expect(a.data).not.toBe(b.data);
  await expect(cli(["start","--id","overlap","--modules","a,b","--","overlap"])).rejects.toThrow();
  await expect(cli(["start","--id","whole","--modules","*","--","whole"])).rejects.toThrow();
  const conversations=JSON.parse(await readFile(`${dir}/conversations.json`,"utf8"));expect(conversations[a.conversation].directory).toBe(a.worktree);
  await writeFile(`${a.worktree}/crates/a/file`,"implemented");await exec("git",["add","."],{cwd:a.worktree});await exec("git",["-c","user.name=Fixture","-c","user.email=fixture@localhost","commit","-m","fixture implementation"],{cwd:a.worktree});
  await writeFile(`${dir}/evidence.txt`,"Fixture checks passed; test review found no unresolved findings.");
  const published=await cli(["publish","one","--evidence",`${dir}/evidence.txt`]);const candidate=published.sessions.one.candidate.commit;
  await expect(cli(["accept","one"])).rejects.toThrow(/human approval/);
  const denied=await page.request.post(`${base}/api/approve`,{headers:{authorization:`Bearer ${token}`},data:{id:"one",commit:candidate}});expect(denied.status()).toBe(403);
  // This automated account approves disposable fixture code, never user work.
  page.once("dialog",d=>d.accept());await page.getByRole("button",{name:"Approve candidate as human"}).click();
  await expect.poll(async()=>Boolean((await cli(["status"])).sessions.one.candidate.approval)).toBe(true);
  await cli(["accept","one"]);expect(await readFile(`${dir}/repo/crates/a/file`,"utf8")).toBe("implemented");
  await expect(page.getByRole("heading",{name:"one · complete",exact:true})).toBeVisible();
  await expect(page.getByRole("link",{name:"first (done)"})).toBeVisible();
  const dependent=(await cli(["start","--id","next","--tickets","dependent","--modules","a","--","dependent now ready"])).session;expect(dependent.phase).toBe("active");
  await cli(["abandon","two"]);await expect(cli(["start","--id","fail","--modules","b","--","failed setup"])).rejects.toThrow(/fixture setup failure/);
  await fetch(`${process.env.FACTORIO_FIXTURE_URL}/restart`);await expect(cli(["start","--id","collision","--modules","b","--","collision"])).rejects.toThrow();
  const failed=(await cli(["status"])).sessions.fail;expect(failed.phase).toBe("starting");await writeFile(`${failed.data}/permit`,"retry fixture hook");
  await cli(["recover","fail"]);expect((await cli(["status"])).sessions.fail.phase).toBe("active");
  await page.reload();await expect(page.getByRole("heading",{name:"fail · active",exact:true})).toBeVisible();
  if(process.env.FACTORIO_TEST_DEV){
    const css=`${root}/apps/factorio/web/style.css`;await writeFile(css,await readFile(css,"utf8")+"\nbody { --factorio-probe: active; }\n");
    await expect.poll(()=>page.evaluate(()=>getComputedStyle(document.body).getPropertyValue("--factorio-probe").trim())).toBe("active");
    const source=`${root}/apps/factorio/src/lib.rs`, original=await readFile(source,"utf8");
    await writeFile(source,original+'\ncompile_error!("fixture failure");\n');
    const state=async()=>await(await fetch(`${process.env.FACTORIO_FIXTURE_URL}/build-state`)).json();
    await expect.poll(async()=>(await state()).failed,{timeout:60000}).toBe(true);
    await page.reload();await expect(page.getByRole("heading",{name:"fail · active",exact:true})).toBeVisible();
    const generations=(await state()).generations;await writeFile(source,original);
    await expect.poll(async()=>(await state()).generations,{timeout:60000}).toBeGreaterThan(generations);
    await expect(page.getByRole("heading",{name:"fail · active",exact:true})).toBeVisible();
    const ui=`${root}/apps/factorio/web/main.tsx`;await writeFile(ui,(await readFile(ui,"utf8")).replace("Local tickets, isolated work and human acceptance.","Updated Factorio development UI."));
    await expect(page.getByText("Updated Factorio development UI.")).toBeVisible();
  }
  await writeFile(`${failed.worktree}/dirty`,"keep");
  await cli(["abandon","fail"]);expect((await cli(["status"])).sessions.fail.phase).toBe("abandoned");expect(await readFile(`${failed.worktree}/dirty`,"utf8")).toBe("keep");
  expect(errors).toEqual([]);
});
