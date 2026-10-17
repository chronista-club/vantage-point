// mem_1Cg1m3LoeDu62UCKVT5UcA
import { expect, it } from 'vitest'
import { buildAtlasTree } from './atlas-tree'
const nodes = [
  {id:'a',name:'A',parentId:null,writable:false},
  {id:'b',name:'B',parentId:'a',writable:true},
  {id:'c',name:'C',parentId:'a',writable:true},
  {id:'d',name:'D',parentId:null,writable:true},
  {id:'apps',name:'Apps',isApps:true,writable:true},
]
const ids = (settings: unknown) => buildAtlasTree(nodes,settings).rows.map(r=>r.node.id)
it('orders siblings, ignores unknown IDs and placement, and retains read-only ancestors',()=>{
  const result=buildAtlasTree(nodes,{version:1,order:{root:['gone','d','a'],a:['c','b']},placement:{b:'root'}})
  expect(result.rows.map(r=>[r.node.id,r.depth])).toEqual([['d',0],['a',0],['c',1],['b',1]])
  expect(result.rows[1].node.writable).toBe(false)
})
it('hides whole subtrees and apps by default',()=>{
  expect(ids({version:1,hidden:['a','missing']})).toEqual(['d'])
})
it('uses the default tree for null and invalid settings, reporting only invalid settings',()=>{
  expect(ids(null)).toEqual(['a','b','c','d'])
  for(const settings of ['broken',{version:2}]) {
    expect(ids(settings)).toEqual(ids(null))
    expect(buildAtlasTree(nodes,settings).invalid).toBe(true)
  }
  expect(buildAtlasTree(nodes,null).invalid).toBe(false)
})
it('ignores malformed individual entries and inherited order keys',()=>{
  expect(ids({version:1,order:{a:42},hidden:'a'})).toEqual(ids(null))
  const special=[{id:'__proto__'},{id:'x',parentId:'__proto__'},{id:'y',parentId:'__proto__'},{id:'constructor'}]
  expect(buildAtlasTree(special,{version:1}).rows.map(r=>r.node.id)).toEqual(['__proto__','x','y','constructor'])
  expect(buildAtlasTree(special,JSON.parse('{"version":1,"order":{"__proto__":["y","x"]}}')).rows.map(r=>r.node.id)).toEqual(['__proto__','y','x','constructor'])
})
it('cuts cycles deterministically, but hides a whole cycle when one member is hidden',()=>{
  const ring=[{id:'z',parentId:'b'},{id:'b',parentId:'a'},{id:'a',parentId:'b'}]
  expect(buildAtlasTree(ring,null).rows.map(r=>[r.node.id,r.depth])).toEqual([['a',0],['b',1],['z',2]])
  expect(buildAtlasTree(ring,{version:1,hidden:['b']}).rows).toEqual([])
})
