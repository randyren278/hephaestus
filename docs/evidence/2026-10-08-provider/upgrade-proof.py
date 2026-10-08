import hashlib, json, os, pathlib, shutil, socket, sqlite3, subprocess, sys, time
root=pathlib.Path(sys.argv[1]); mode=sys.argv[2]; data=root/'data'; repo=root/'source'
old=root/'legacy-target/debug'; new=pathlib.Path.cwd()/'target/debug'
bins=old if mode=='legacy' else new
def invoke(*args, text=False):
    command=[str(bins/'hephaestus'),'--data-dir',str(data)]
    if not text: command+=['--json']
    result=subprocess.run(command+list(args),text=True,capture_output=True,timeout=60)
    if result.returncode: raise RuntimeError(result.stderr or result.stdout)
    if text: return result.stdout.split()[0]
    response=json.loads(result.stdout)
    assert response.get('error') is None, response.get('error')
    return response['data']
def binary_hashes():
    hashes={name:hashlib.sha256((bins/name).read_bytes()).hexdigest() for name in ['hephaestus','hephaestusd']}
    hashes.update({name:hashlib.sha256((data/name).read_bytes()).hexdigest() for name in ['reference-evaluator','reference-worker']})
    return hashes
def assert_arena_version(id,version):
    status=invoke('job','status',id)
    assert status['type']=='evaluation' and status['evaluation']['evaluation_id']==id,status
    with sqlite3.connect(f'file:{data}/events.sqlite3?mode=ro',uri=True) as connection:
        rows=connection.execute("SELECT payload FROM events WHERE event_type='arena.job.admitted'").fetchall()
    record=next(json.loads(row[0]) for row in rows if json.loads(row[0])['evaluation_id']==id)
    assert record['environment_id'].startswith(f'provider-v{version}.'),record['environment_id']
    assert record.get('candidate_environment_id') is None
    return {'status':status,'admitted_environment_id':record['environment_id'],'candidate_environment_id':record.get('candidate_environment_id')}
def admissions():
    with sqlite3.connect(f'file:{data}/events.sqlite3?mode=ro',uri=True) as connection:
        return connection.execute("SELECT count(*) FROM events WHERE event_type IN ('job.admitted','arena.job.admitted','run.result_recorded')").fetchone()[0]
def reject_invalid_family(args):
    before=admissions()
    result=subprocess.run([str(bins/'hephaestus'),'--data-dir',str(data),'--json']+args,text=True,capture_output=True,timeout=60)
    assert result.returncode!=0
    response=json.loads(result.stdout)
    assert response['error']['message']=='registered provider model is invalid',response['error']
    assert admissions()==before,'invalid model admitted work'
    return response['error']
if mode=='legacy':
    repo.mkdir(); (repo/'fixture.txt').write_text('Provider upgrade fixture\n')
    for args in [['init','-q'],['config','user.name','Upgrade Fixture'],['config','user.email','upgrade@example.invalid'],['add','.'],['commit','-m','fixture','-q']]:
        subprocess.run(['git','-C',str(repo)]+args,check=True,capture_output=True)
    data.mkdir()
    for name in ['evaluator','worker']:
        shutil.copyfile(old/f'hephaestus-reference-{name}',data/f'reference-{name}')
        (data/f'reference-{name}').chmod(0o700)
    (root/'fake-claude').write_text('#!/bin/sh\ncat >/dev/null\nprintf \'%s\\n\' \'{"type":"result","subtype":"success","result":"ANSWER","total_cost_usd":0.001}\'\n')
    (root/'fake-claude').chmod(0o700)
log=(root/f'{mode}-daemon.log').open('w')
environment=os.environ.copy(); environment['HEPHAESTUS_CLAUDE_EXECUTABLE']=str(root/'fake-claude')
process=subprocess.Popen([str(bins/'hephaestusd'),'--data-dir',str(data),'--source-repository',str(repo),'--evaluator-executable',str(data/'reference-evaluator'),'--reference-worker-executable',str(data/'reference-worker')],stdout=log,stderr=log,env=environment)
try:
    deadline=time.monotonic()+15
    while True:
        assert process.poll() is None, f'daemon exited ({mode}); inspect private log'
        try:
            with socket.socket(socket.AF_UNIX) as connection: connection.connect(str(data/'control.sock'))
            break
        except OSError:
            assert time.monotonic()<deadline
            time.sleep(.05)
    if mode=='legacy':
        ids={}
        for visibility in ['visible','sealed']:
            path=root/f'{visibility}.json'
            path.write_text(json.dumps({'schema_version':1,'manifest_id':f'upgrade-{visibility}','visibility':visibility,'tasks':[{'task_id':f'{visibility}-task','input':visibility,'expected_output':'ANSWER'}]}))
            ids[visibility]=invoke('arena','manifest',str(path),text=True)
        ids['evaluator']=invoke('artifact','put',str(data/'reference-evaluator'),text=True)
        ids['verifier']=invoke('verifier',text=True)
        world={'schema_version':1,'name':'provider-upgrade','laws':{'candidate_network':False,'candidate_evaluator_access':False,'maximum_cost_microusd':1000000},'authority_ceiling':{'workspace_write':False,'network':False},'mutation_scope':[],'promotion':{'minimum_delta_bps':0,'maximum_regressions':0,'confidence_bps':9500},'objectives':['correctness'],'evaluator_artifacts':{'arena.visible_manifest':ids['visible'],'arena.sealed_manifest':ids['sealed'],'arena.evaluator':ids['evaluator'],'arena.runtime_verifier':ids['verifier']}}
        path=root/'world.json'; path.write_text(json.dumps(world))
        ids['world']=invoke('world','register',str(path))['world']['world_id']
        for role in ['parent','candidate','invalid']:
            genome={'schema_version':1,'name':f'upgrade-{role}','parents':[],'model':{'provider':'claude','family':'historical model label' if role=='invalid' else 'sonnet'},'authority':{'workspace_write':False,'network':False},'artifacts':{}}
            path=root/f'{role}.json';path.write_text(json.dumps(genome))
            ids[role]=invoke('genome','register',str(path),'--world',ids['world'])['genome']['genome_id']
        (root/'ids.json').write_text(json.dumps(ids))
        invoke('unfreeze')
        legacy_invalid_run=invoke('run',ids['invalid'])
        job=invoke('submit','upgrade-v1-submit',ids['candidate'])
        deadline=time.monotonic()+20
        while True:
            job=invoke('job','status','upgrade-v1-submit')
            state=job['job']['state']
            if state not in ['admitted','running','cancellation_requested']: break
            assert time.monotonic()<deadline
            time.sleep(.05)
        assert state=='succeeded',job
        assert job['job']['environment_id'].startswith('provider-v1.'),job
        evaluation=invoke('arena','evaluate','upgrade-v1-eval',ids['parent'],ids['candidate'])
        replay=invoke('replay')
        report={'legacy_commit':'7bf55d0','binary_sha256':binary_hashes(),'v1_job':job,'v1_evaluation':evaluation,'v1_arena_job':assert_arena_version('upgrade-v1-eval',1),'v1_replay':replay,'invalid_family_genome':ids['invalid'],'legacy_invalid_family_run':legacy_invalid_run}
    else:
        ids=json.loads((root/'ids.json').read_text())
        old_hashes=json.loads((root/'legacy-report.json').read_text())['binary_sha256']
        for name in ['reference-evaluator','reference-worker']:
            assert binary_hashes()[name]==old_hashes[name],f'{name} changed during upgrade'
        replay=invoke('replay')
        old_job=invoke('job','status','upgrade-v1-submit')
        assert old_job['job']['state']=='succeeded'
        assert old_job['job']['environment_id'].startswith('provider-v1.')
        evaluations=invoke('evaluations','--limit','10')
        old_arena=assert_arena_version('upgrade-v1-eval',1)
        invalid=invoke('genome','show',ids['invalid'])
        invalid_rejections=[reject_invalid_family(args) for args in [
            ['run',ids['invalid']],
            ['submit','invalid-family-submit',ids['invalid']],
            ['arena','evaluate','invalid-family-eval',ids['invalid'],ids['candidate']],
        ]]
        invoke('submit','upgrade-v2-submit',ids['candidate'])
        deadline=time.monotonic()+20
        while True:
            job=invoke('job','status','upgrade-v2-submit')
            state=job['job']['state']
            if state not in ['admitted','running','cancellation_requested']:break
            assert time.monotonic()<deadline
            time.sleep(.05)
        assert state=='succeeded',job
        assert job['job']['environment_id'].startswith('provider-v2.'),job
        evaluation=invoke('arena','evaluate','upgrade-v2-eval',ids['parent'],ids['candidate'])
        report={'legacy_commit':'7bf55d0','binary_sha256':binary_hashes(),'legacy_invalid_family_replayed':invalid,'invalid_family_launch_rejections':invalid_rejections,'v1_arena_job_after_upgrade':old_arena,'v2_arena_job':assert_arena_version('upgrade-v2-eval',2),'old_job_after_upgrade':old_job,'replay_before_new_work':replay,'old_evaluations_after_upgrade':evaluations,'v2_job':job,'v2_evaluation':evaluation,'mixed_version_replay':invoke('replay')}
    (root/f'{mode}-report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({'mode':mode,'job_state':state,'environment':job['job']['environment_id'],'report_saved':True}))
finally:
    if process.poll() is None:
        try:invoke('daemon','stop')
        except Exception:process.terminate()
        try:process.wait(timeout=15)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    log.close()
