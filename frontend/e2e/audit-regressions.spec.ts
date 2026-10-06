import { expect,test,type Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { aggregateCacheHit } from '../src/features/portal-overview/cache-metrics'
import { apiFetch,setKey,clearKey,registerAuthReset } from '../src/lib/api'
import { QueryClient } from '@tanstack/react-query'

async function prepare(page:Page) {
  await page.addInitScript(()=>{localStorage.setItem('okapi.key','audit-fixture');localStorage.setItem('okapi.lang','zh-CN')})
  await page.route('**/*',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname
    if(request.isNavigationRequest()) return route.fulfill({path:fileURLToPath(new URL('../dist/index.html',import.meta.url)),contentType:'text/html'})
    if(!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    expect(request.method(),`unmocked write ${path}`).toBe('GET')
    return route.fulfill({json:path==='/api/me'?{user_id:1,key_id:1,group:'default',balance_micro:0,role:100,permissions:['*']}:path==='/api/notice'?{notice:null}:path.startsWith('/admin/settings/')?{value:null}:{data:[],total:0}})
  })
}

test('A24 account changes clear caches and reject a delayed response from the prior account',async()=>{
  const originalFetch=globalThis.fetch
  const descriptor=Object.getOwnPropertyDescriptor(globalThis,'localStorage')
  const values=new Map<string,string>()
  Object.defineProperty(globalThis,'localStorage',{configurable:true,value:{getItem:(k:string)=>values.get(k)??null,setItem:(k:string,v:string)=>values.set(k,v),removeItem:(k:string)=>values.delete(k)}})
  const client=new QueryClient();const unregister=registerAuthReset(()=>client.clear())
  let reply!:(value:Response)=>void
  globalThis.fetch=()=>new Promise(resolve=>{reply=resolve})
  try {
    setKey('account-a');client.setQueryData(['private'],{owner:'account-a'})
    const pending=apiFetch('/private')
    clearKey();setKey('account-b')
    expect(client.getQueryData(['private'])).toBeUndefined()
    reply(new Response(JSON.stringify({owner:'account-a'}),{headers:{'content-type':'application/json'}}))
    await expect(pending).rejects.toMatchObject({code:'auth_context_changed'})
  } finally {
    unregister();globalThis.fetch=originalFetch
    if(descriptor) Object.defineProperty(globalThis,'localStorage',descriptor)
    else Reflect.deleteProperty(globalThis,'localStorage')
  }
})

test('A52 partial cache hit rates use paired samples and keep unknown populations out of the denominator',()=>{
  const known={requests:1,prompt_tokens:100,cached_tokens:50,cache_hit_bp:5000}
  const unknown={requests:1,prompt_tokens:900,cached_tokens:0,cache_hit_bp:null}
  expect(aggregateCacheHit([known,unknown])).toEqual({bp:5000,samples:1,partial:true})
  expect(aggregateCacheHit([unknown])).toEqual({bp:null,samples:0,partial:true})
  const partial={requests:4,prompt_tokens:1000,cached_tokens:50,cache_hit_bp:null,measured_cache_hit_bp:5000,measured_cache_hit_requests:1,measured_prompt_tokens:100,measured_cache_read_tokens:50}
  expect(aggregateCacheHit([partial])).toEqual({bp:5000,samples:1,partial:true})
})

test('A50 editing a disabled scheduled rule preserves status and both validity boundaries',async({page})=>{
  await prepare(page)
  const original={rule_code:'scheduled-rule',rule_type:'time_based',priority:5,enabled:false,valid_from:'2099-01-01T00:00:00Z',valid_to:'2099-12-31T23:59:59Z',scope:{},params:{multiplier:'0.5',start_minute:0,end_minute:359,weekdays:[1],stacking_mode:'exclusive'}}
  await page.route('**/admin/pricing/rules?*',route=>route.fulfill({json:{data:[original],total:1}}))
  let saved:Record<string,unknown>|undefined
  await page.route('**/admin/pricing/rules',async route=>{saved=route.request().postDataJSON();await route.fulfill({json:{ok:true}})})
  await page.goto('/admin/rules')
  await page.getByRole('row').filter({hasText:'scheduled-rule'}).getByRole('button',{name:'编辑',exact:true}).click()
  const drawer=page.getByRole('dialog');await drawer.locator('#r-mult').fill('0.6')
  await drawer.getByRole('button',{name:'保存',exact:true}).click()
  await expect.poll(()=>saved).toMatchObject({enabled:false,valid_from:original.valid_from,valid_to:original.valid_to,multiplier:'0.6'})
})

test('A51 stale refund lookups cannot authorize a different request and confirmation keeps an immutable target',async({page})=>{
  await prepare(page)
  let resume!:()=>void,entered!:()=>void
  const arrived=new Promise<void>(resolve=>{entered=resolve}),gate=new Promise<void>(resolve=>{resume=resolve})
  await page.route('**/admin/billing/record/**',async route=>{
    const id=new URL(route.request().url()).pathname.split('/').pop()!
    if(id==='request-a'){entered();await gate}
    await route.fulfill({json:{request_id:id,user_id:7,username:'alice',model:'fixture',status:20,amount_micro:1000000,prompt_tokens:10,completion_tokens:10,error_code:null,created_at:'2026-09-08T00:00:00Z',refundable:true}})
  })
  let operation:unknown
  await page.route('**/admin/billing/refund',async route=>{operation=route.request().postDataJSON();await route.fulfill({json:{outcome:'already_refunded'}})})
  await page.goto('/admin/ops');await page.locator('#rid').fill('request-a')
  await page.getByRole('button',{name:'查询这笔账'}).click();await arrived
  await page.locator('#rid').fill('request-b');resume()
  await expect(page.getByRole('button',{name:'查询这笔账'})).toBeEnabled()
  await expect(page.getByRole('button',{name:'退款',exact:true})).toHaveCount(0)
  await page.getByRole('button',{name:'查询这笔账'}).click()
  await page.locator('#rreason').fill('immutable reason')
  await page.getByRole('button',{name:'退款',exact:true}).click()
  // A background form edit while a modal is open must not alter its operation.
  await page.locator('#rid').evaluate((node:HTMLInputElement)=>{
    const setter=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value')!.set!
    setter.call(node,'request-c');node.dispatchEvent(new Event('input',{bubbles:true}))
  })
  await page.getByRole('alertdialog').getByRole('button',{name:'退款',exact:true}).click()
  await expect.poll(()=>operation).toEqual({request_id:'request-b',reason:'immutable reason'})
})

test('A53 admin log date filters retain seconds and milliseconds after applying another filter',async({page})=>{
  await prepare(page)
  const from='2026-09-20T01:02:03.789Z',to='2026-09-21T04:05:06.123Z'
  const queries:URLSearchParams[]=[]
  await page.route('**/admin/logs?*',route=>{if(route.request().isNavigationRequest()) return route.fallback();queries.push(new URL(route.request().url()).searchParams);return route.fulfill({json:{data:[]}})})
  await page.goto(`/admin/logs?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`)
  await expect(page.locator('#logs-from')).toBeVisible()
  expect(new Date(await page.locator('#logs-from').inputValue()).toISOString()).toBe(from)
  expect(new Date(await page.locator('#logs-to').inputValue()).toISOString()).toBe(to)
  await page.getByRole('button',{name:'应用区间',exact:true}).click()
  await expect.poll(()=>queries.at(-1)?.get('from')).toBe(from)
  await expect.poll(()=>queries.at(-1)?.get('to')).toBe(to)
})

test('A08 signed refund flows keep negative detail rows and finish loading', async ({page}) => {
  await prepare(page)
  await page.route('**/admin/stats/flow?*', route => route.fulfill({json:{
    days:7,metric:'amount',scope:{},stages:['user','model'],total:-1000000,coverage_bp:10000,truncated:false,
    nodes:[{id:'user:7',stage:'user',key:'7',label:'Refund user',name:'Refund user',value:-1000000,other:false},
      {id:'model:fixture',stage:'model',key:'fixture',label:'fixture',name:'fixture',value:-1000000,other:false}],
    links:[{source:'user:7',target:'model:fixture',value:-1000000}],
  }}))
  await page.goto('/admin/stats?view=flow')
  await expect(page.getByText('存在退款等负值，请查看下方明细；流向图无法表示负金额。')).toBeVisible()
  await expect(page.getByRole('status')).toHaveCount(0)
  await expect(page.getByRole('table')).toBeVisible()
  await expect(page.getByRole('table')).toContainText('fixture')
  await expect(page.getByRole('table')).toContainText('-US$1.00')
})
