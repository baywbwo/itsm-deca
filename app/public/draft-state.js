// RAM only. Intents outlive route/form reconstruction and clear only on receipt/logout.
export function createDraftState(randomKey=()=>crypto.randomUUID()){
 const drafts=new Map(),intents=new Map();
 return {drafts,intent(key){if(!intents.has(key))intents.set(key,randomKey());return intents.get(key);},received(key){intents.delete(key);drafts.delete(key);},clear(){drafts.clear();intents.clear();},get dirty(){return drafts.size>0||intents.size>0;}};
}
