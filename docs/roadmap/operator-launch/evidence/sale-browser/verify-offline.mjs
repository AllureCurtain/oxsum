import {readFileSync,writeFileSync,readdirSync} from 'node:fs';
import {verifyBill, verifyPriceHead, verifySaleHead} from 'verify-oxsum-bill';
import {inspect} from './scope.mjs';
const root=new URL('.', import.meta.url).pathname;
let fetchAttempts=0;globalThis.fetch=()=>{fetchAttempts++;throw new Error('Offline verification forbids fetch');};
const results=[];
for(const name of ['customer-b','pinned-a','changed-a','unchanged-b','pinned-provider']){
 const bills=JSON.parse(readFileSync(root+'/'+name+'-bills-after.json','utf8')).data.rows;
 const files=readdirSync(root).filter(f=>f.startsWith(name+'-proof-'));if(files.length!==1)throw new Error('Ambiguous proof');
 const raw=inspect(readFileSync(root+'/'+files[0],'utf8')).token(['data']);
 const bundle=JSON.parse(raw);
 const bill=bills.find(x=>x.entryId===bundle.entry.id);if(!bill)throw new Error('Missing independently retained bill hash');
 const verdict=await verifyBill(raw,bill.contentHash);
 if(!verdict.ok)throw new Error(JSON.stringify(verdict));
 const price=await verifyPriceHead(raw); const sale=await verifySaleHead(raw); if(!price.ok||!sale.ok)throw new Error(JSON.stringify({price,sale}));
 const bad=await verifyBill(raw,'0'.repeat(64));if(bad.ok)throw new Error('Wrong hash accepted');
 const tampered=structuredClone(bundle);const d=JSON.parse(tampered.entry.description);d.charged++;tampered.entry.description=JSON.stringify(d);
 const altered=await verifyBill(JSON.stringify(tampered),bill.contentHash);if(altered.ok)throw new Error('Altered charge accepted');
 results.push({name,entryId:bill.entryId,retainedHash:bill.contentHash,verdict,price,sale,wrongHash:bad,alteredCharge:altered});
}
writeFileSync(root+'/offline-verdicts.json',JSON.stringify({fetchAttempts,results},null,2)+'\n');console.log(JSON.stringify({fetchAttempts,results},null,2));
