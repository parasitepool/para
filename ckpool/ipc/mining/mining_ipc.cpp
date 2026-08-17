/*
 * Copyright (c) 2026 Con Kolivas
 *
 * C++ implementation of the Bitcoin Core mining IPC C interface declared in
 * mining_ipc.h. All Cap'n Proto / kj complexity is confined to this file so
 * the rest of ckpool stays pure C.
 *
 * Transport: the Bitcoin Core IPC socket speaks Cap'n Proto RPC using the
 * libmultiprocess conventions. We talk to it directly with capnp's EzRpcClient
 * and the generated client stubs, performing the libmultiprocess handshake by
 * hand: exchange thread maps via Init.construct(), then supply a
 * Proxy.Context{thread, callbackThread} on every Mining call. This avoids a
 * build dependency on libmultiprocess itself.
 *
 * Threading: an EzRpcClient runs its kj event loop on the thread that
 * constructs it, and every call on it MUST be made from that same thread.
 * A single context is therefore created, connected and used entirely from one
 * caller thread (ckpool's ipcnotify thread). This is sufficient for the Phase 1
 * tip-notification surface; wider cross-thread use (block template generation)
 * will route calls through the event loop in a later phase.
 *
 * Scope: tip notification; template service (createNewBlock / submitSolution);
 * and Mining.checkBlock for SV2 JD candidate-block validation.
 */

#include "mining_ipc.h"

#include <capnp/ez-rpc.h>
#include <capnp/rpc-twoparty.h>
#include <kj/async-io.h>
#include <kj/exception.h>
#include <kj/time.h>

#include "init.capnp.h"
#include "mining.capnp.h"
#include "common.capnp.h"

#include <atomic>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <sys/stat.h>
#include <unistd.h>

#include <condition_variable>
#include <mutex>
#include <stdexcept>
#include <thread>

namespace {

using namespace ipc::capnp::messages;

/* Minimal Thread / ThreadMap server implementation. bitcoind requires a valid
 * thread handle in every Proxy.Context; we serve a ThreadMap so it can create
 * callback thread handles that route back to us. */
class ThreadServer final : public mp::Thread::Server {
public:
	explicit ThreadServer(kj::StringPtr name) : name_(kj::str(name)) {}

	kj::Promise<void> getName(GetNameContext context) override
	{
		/* Concrete MessageSize (not empty Maybe/nullptr): GCC -Wmaybe-uninitialized
		 * false-positives on kj::Maybe copy of an unset MessageSize in getResults. */
		capnp::MessageSize hint{1, 32};
		context.getResults(hint).setResult(name_);
		return kj::READY_NOW;
	}

private:
	kj::String name_;
};

class ThreadMapServer final : public mp::ThreadMap::Server {
public:
	kj::Promise<void> makeThread(MakeThreadContext context) override
	{
		auto name = context.getParams().getName();
		/* Concrete MessageSize: see ThreadServer::getName. */
		capnp::MessageSize hint{1, 64};
		context.getResults(hint).setResult(mp::Thread::Client(kj::heap<ThreadServer>(name)));
		return kj::READY_NOW;
	}
};

/* Convert a 64 char hex string to 32 bytes. Returns false on malformed input. */
static bool hex_to_bin(const char *hex, uint8_t *out, size_t bytes)
{
	for (size_t i = 0; i < bytes; ++i) {
		char c[3] = { hex[i * 2], hex[i * 2 + 1], 0 };
		if (!isxdigit((unsigned char)c[0]) || !isxdigit((unsigned char)c[1]))
			return false;
		out[i] = (uint8_t)strtol(c, nullptr, 16);
	}
	return true;
}

/* Fill a mining_tip from a BlockRef reader. Returns 0 on success. */
static int fill_tip(BlockRef::Reader ref, mining_tip *out)
{
	auto hash = ref.getHash();

	if (hash.size() != 32)
		return -1;
	static const char *hexd = "0123456789abcdef";
	for (size_t i = 0; i < 32; ++i) {
		uint8_t b = hash[i];
		out->hash[i * 2]     = hexd[b >> 4];
		out->hash[i * 2 + 1] = hexd[b & 0x0f];
	}
	out->hash[64] = '\0';
	out->height = ref.getHeight();
	return 0;
}

class MiningIpc {
public:
	explicit MiningIpc(std::string socket_path) : socket_path_(std::move(socket_path)) {}

	~MiningIpc() { teardown(); }

	/* Establish the transport and handshake, then attempt to obtain a Mining
	 * capability. Returns false if the socket is absent or the transport
	 * could not be built; a node that is up but not yet ready to mine still
	 * returns true (obtain is retried later). */
	bool connect()
	{
		if (connected_.load())
			return true;

		struct stat st;
		if (stat(socket_path_.c_str(), &st) != 0)
			return false;

		try {
			client_ = std::make_unique<capnp::EzRpcClient>("unix:" + socket_path_);
			auto &ws = client_->getWaitScope();
			auto init = client_->getMain<Init>();

			/* Our ThreadMap and the callback thread bitcoind delivers
			 * long-lived results (e.g. waitTipChanged) on. */
			our_threadmap_ = mp::ThreadMap::Client(kj::heap<ThreadMapServer>());
			{
				auto mk = our_threadmap_.makeThreadRequest();
				mk.setName("ckpool-callback");
				callback_thread_ = mk.send().wait(ws).getResult();
			}

			/* Exchange thread maps, then create the server-side execution
			 * thread that every Mining call runs on. */
			{
				auto req = init.constructRequest();
				req.setThreadMap(our_threadmap_);
				server_threadmap_ = req.send().wait(ws).getThreadMap();
			}
			{
				auto mk = server_threadmap_.makeThreadRequest();
				mk.setName("bitcoin-execution");
				exec_thread_ = mk.send().wait(ws).getResult();
			}

			have_threads_ = true;
			connected_.store(true);
		} catch (const kj::Exception &e) {
			client_.reset();
			return false;
		}

		/* Obtaining Mining may legitimately fail while the node is still
		 * starting up; that is not a transport failure. */
		obtain_mining();
		return true;
	}

	/* Reconnect if needed, then ensure a usable Mining client. Returns true
	 * once mining is available. */
	bool try_obtain_mining()
	{
		if (!connected_.load()) {
			teardown();
			if (!connect())
				return false;
		}
		return have_mining_ || obtain_mining();
	}

	int get_tip(mining_tip *out)
	{
		if (!connected_.load() || !have_mining_ || !out)
			return -1;
		try {
			auto &ws = client_->getWaitScope();
			auto req = mining_.getTipRequest();
			fill_context(req.initContext());
			auto resp = req.send().wait(ws);
			if (!resp.getHasResult())
				return -1;
			return fill_tip(resp.getResult(), out);
		} catch (const kj::Exception &) {
			/* getTip has no timeout and does not throw on success, so any
			 * exception means the connection is gone. Force a reconnect
			 * regardless of the reported exception type. */
			connected_.store(false);
			return -1;
		}
	}

	int wait_tip_changed(const char *current_tip_hex, double timeout_seconds, mining_tip *out)
	{
		if (!connected_.load() || !have_mining_ || !current_tip_hex || !out)
			return -1;
		try {
			uint8_t tip[32];
			if (!hex_to_bin(current_tip_hex, tip, 32))
				return -1;

			auto &ws = client_->getWaitScope();
			auto req = mining_.waitTipChangedRequest();
			fill_context(req.initContext());
			req.setCurrentTip(kj::arrayPtr(reinterpret_cast<const capnp::byte *>(tip), 32));
			req.setTimeout(timeout_seconds > 0 ? timeout_seconds : 1e9);

			auto resp = req.send().wait(ws);
			return fill_tip(resp.getResult(), out);
		} catch (const kj::Exception &e) {
			/* bitcoind signals a genuine timeout by returning the current
			 * tip, not by throwing, so any exception here is a failure. A
			 * lost connection is not always reported as DISCONNECTED (the
			 * failure can surface through the proxied execution thread as a
			 * generic error), so treat any non-timeout exception as a
			 * dropped connection and force a reconnect. */
			if (e.getType() != kj::Exception::Type::DISCONNECTED) {
				const char *desc = e.getDescription().cStr();
				if (strstr(desc, "timeout") || strstr(desc, "Timeout"))
					return -1;
			}
			connected_.store(false);
			return -1;
		}
	}

private:
	/* Every Mining call carries a Proxy.Context routing it to the server
	 * execution thread with results delivered on our callback thread. */
	template <typename CtxBuilder>
	void fill_context(CtxBuilder ctx)
	{
		ctx.setThread(exec_thread_);
		ctx.setCallbackThread(callback_thread_);
	}

	/* Flag the connection as lost on a disconnect so the next
	 * try_obtain_mining() rebuilds it. */
	void note_disconnect(const kj::Exception &e)
	{
		if (e.getType() == kj::Exception::Type::DISCONNECTED)
			connected_.store(false);
	}

	/* Obtain and validate a Mining capability on the current transport. */
	bool obtain_mining()
	{
		if (!client_ || !have_threads_)
			return false;
		try {
			auto &ws = client_->getWaitScope();
			auto init = client_->getMain<Init>();

			auto req = init.makeMiningRequest();
			fill_context(req.initContext());
			mining_ = req.send().wait(ws).getResult();

			/* A null Mining capability only errors on a real call, so
			 * probe it before declaring success. */
			auto probe = mining_.isInitialBlockDownloadRequest();
			fill_context(probe.initContext());
			probe.send().wait(ws);

			have_mining_ = true;
			return true;
		} catch (const kj::Exception &e) {
			note_disconnect(e);
			have_mining_ = false;
			return false;
		}
	}

	/* Reset all per-connection state so a subsequent connect() rebuilds it
	 * from scratch. The capability handles below reference the dead client
	 * and become harmless broken capabilities until overwritten. */
	void teardown()
	{
		connected_.store(false);
		have_mining_ = false;
		have_threads_ = false;
		/* The capability handles below are imported from (or exported to)
		 * client_'s RpcSystem and hold hooks into it. They MUST be
		 * destroyed while that RpcSystem is still alive, so drop them
		 * before resetting client_ — otherwise the later destruction of a
		 * dangling cap dereferences the freed RpcSystem and crashes. This
		 * is the reconnect path (bitcoind restarting) where these are
		 * live. */
		mining_ = Mining::Client(nullptr);
		exec_thread_ = mp::Thread::Client(nullptr);
		callback_thread_ = mp::Thread::Client(nullptr);
		server_threadmap_ = mp::ThreadMap::Client(nullptr);
		our_threadmap_ = mp::ThreadMap::Client(nullptr);
		client_.reset();
	}

	std::string socket_path_;
	std::atomic<bool> connected_{false};
	bool have_threads_ = false;
	bool have_mining_ = false;

	std::unique_ptr<capnp::EzRpcClient> client_;
	Mining::Client mining_{nullptr};

	mp::ThreadMap::Client our_threadmap_{nullptr};
	mp::ThreadMap::Client server_threadmap_{nullptr};
	mp::Thread::Client callback_thread_{nullptr};
	mp::Thread::Client exec_thread_{nullptr};
};

/* --------------------------------------------------------------------------
 * Phase 2: the "service" connection for block template generation.
 *
 * An EzRpcClient is thread affine, but template operations are driven from
 * several ckpool threads (block_update, share submission). We therefore own
 * the client on a dedicated internal thread running the kj event loop, and
 * marshal every call onto it with kj::Executor::executeSync(), which accepts a
 * promise-returning functor and drives the loop until it resolves. Callers on
 * any (non-kj) thread block until the result is available.
 * -------------------------------------------------------------------------- */

struct BlockTemplateHolder; // defined after MiningService

class MiningService {
public:
	explicit MiningService(std::string socket_path) : socket_path_(std::move(socket_path)) {}

	~MiningService()
	{
		/* The capnp capability members reference the event loop owned by the
		 * service thread and must be destroyed there, not on the caller's
		 * thread. Reset them on the service thread; the member destructors
		 * that then run on this thread are no-ops on null capabilities. Only
		 * attempt this while connected: if the service thread is looping in
		 * reconnect (not in the event loop) executeSync would block until it
		 * returned, potentially forever if bitcoind is down. */
		/* Before reset_caps(), so the keepalive loop this is about to
		 * unpark sees the stop and returns rather than reconnecting into a
		 * service that is being destroyed. */
		stop_.store(true);
		if (executor_ && connected_.load()) {
			try {
				executor_->executeSync([this]() { reset_caps(); });
			} catch (...) {
			}
		}
		/* The service thread runs a kj loop for the process lifetime; detach
		 * rather than join to avoid blocking on the cross-thread wakeup. */
		if (thread_.joinable())
			thread_.detach();
	}

	/* Start the background thread and block until its first connect attempt
	 * has completed. Returns true if the thread started (regardless of
	 * whether mining is yet available). */
	bool start()
	{
		thread_ = std::thread([this] { thread_main(); });
		std::unique_lock<std::mutex> lock(mutex_);
		cv_.wait(lock, [this] { return started_; });
		return true;
	}

	bool ready() const { return have_mining_.load(); }

	/* Marshal a promise-returning functor onto the service thread and block
	 * for its result. Must only be called once started_ is set. */
	template <typename Func>
	auto run(Func &&func) -> decltype(std::declval<kj::Executor>().executeSync(std::declval<Func>()))
	{
		return executor_->executeSync(kj::fwd<Func>(func));
	}

	/* Fill a Proxy.Context for a Mining/BlockTemplate call. Runs on the
	 * service thread. */
	template <typename CtxBuilder>
	void fill_context(CtxBuilder ctx)
	{
		ctx.setThread(exec_thread_);
		ctx.setCallbackThread(callback_thread_);
	}

	int create_new_block(BlockTemplateHolder **out);

	Mining::Client &mining() { return mining_; }

private:
	/* Runs on the service thread: connect, handshake, then drive the loop
	 * forever, reconnecting if the connection drops or the node is absent. */
	void thread_main()
	{
		bool signalled = false;
		/* One event loop for the whole thread lifetime. Cross-thread
		 * callers marshal template calls onto executor_, which is bound to
		 * this loop; recreating the loop per connection (as EzRpcClient
		 * does) would leave executor_ dangling after the first reconnect
		 * and crash the next executeSync. So we own a persistent loop here
		 * and rebuild only the transport (TwoPartyClient over a fresh
		 * socket) on each connect. */
		auto io = kj::setupAsyncIo();
		auto &ws = io.waitScope;
		executor_ = &kj::getCurrentThreadExecutor();

		while (!stop_.load()) {
			try {
				struct stat st;
				if (stat(socket_path_.c_str(), &st) != 0)
					throw std::runtime_error("socket absent");

				/* Fresh transport for this attempt. conn is declared
				 * before client so it outlives the TwoPartyClient that
				 * references it. */
				auto addr = io.provider->getNetwork()
						.parseAddress("unix:" + socket_path_).wait(ws);
				auto conn = addr->connect().wait(ws);
				capnp::TwoPartyClient client(*conn);
				/* Drop the caps derived from this client on any scope
				 * exit while it is still alive (this defer runs before
				 * client destructs), so a disconnect/reconnect never
				 * leaves a dangling cap referencing a freed RpcSystem. */
				KJ_DEFER(reset_caps());

				auto init = client.bootstrap().castAs<Init>();
				handshake(ws, init);

				connected_.store(true);
				signal_started(signalled);

				/* Drive the loop, probing periodically so a dropped
				 * connection is actually detected. Unlike the notifier
				 * (which always has an in-flight waitTipChanged that
				 * throws on disconnect), this thread has no outstanding
				 * call, so kj::NEVER_DONE.wait() would park forever and
				 * a restarted bitcoind would go unnoticed — leaving
				 * template generation stuck on the getblocktemplate
				 * fallback. The timed wait also runs the event loop, so
				 * the cross-thread executeSync calls that drive template
				 * generation continue to be serviced between probes. */
				auto &timer = io.provider->getTimer();
				while (!stop_.load()) {
					timer.afterDelay(5 * kj::SECONDS).wait(ws);
					/* The wait runs the event loop, so a cross thread
					 * reset_caps() (destructor) can have landed while we
					 * were parked. Re-test before touching any cap. */
					if (stop_.load())
						break;
					/*
					 * Without the Mining capability there is nothing to
					 * probe with — handshake() leaves mining_ null when
					 * the node took our connection but was not yet
					 * serving Mining — so retry obtaining it instead of
					 * calling through a null cap. This is the "retry on
					 * the next call" that handshake() promises, and it is
					 * the same live round trip, so the disconnect
					 * detection below is not weakened.
					 */
					if (!have_mining_.load()) {
						obtain_mining(ws, init);
						continue;
					}
					/* A live round-trip: returns on success (even during
					 * IBD), throws if the connection has dropped, which
					 * unwinds to the reconnect path below. */
					auto probe = mining_.isInitialBlockDownloadRequest();
					fill_context(probe.initContext());
					probe.send().wait(ws);
				}
			} catch (...) {
				connected_.store(false);
				have_mining_.store(false);
				signal_started(signalled);
			}
			if (stop_.load())
				break;
			/* Socket absent or connection lost: wait before retrying so
			 * we do not spin while bitcoind is down. */
			usleep(1000000);
		}
	}

	/* Perform the ThreadMap exchange + obtain a Mining capability. Runs on
	 * the service thread; throws on transport failure. */
	void handshake(kj::WaitScope &ws, Init::Client &init)
	{
		our_threadmap_ = mp::ThreadMap::Client(kj::heap<ThreadMapServer>());
		{
			auto mk = our_threadmap_.makeThreadRequest();
			mk.setName("ckpool-svc-callback");
			callback_thread_ = mk.send().wait(ws).getResult();
		}
		{
			auto req = init.constructRequest();
			req.setThreadMap(our_threadmap_);
			server_threadmap_ = req.send().wait(ws).getThreadMap();
		}
		{
			auto mk = server_threadmap_.makeThreadRequest();
			mk.setName("bitcoin-svc-execution");
			exec_thread_ = mk.send().wait(ws).getResult();
		}

		obtain_mining(ws, init);
	}

	/*
	 * Obtain the Mining capability, the one part of the handshake that may
	 * legitimately fail against a node that has accepted our connection but is
	 * still starting up. That is not fatal: mining_ is left null and
	 * have_mining_ false, ready() reports not ready so callers fall back, and
	 * the keepalive loop retries this on its next pass.
	 *
	 * A dropped connection is different, and must not be mistaken for a node
	 * that is merely not ready yet, or the loop would retry forever against a
	 * dead transport and a restarted bitcoind would go unnoticed. capnp reports
	 * that as DISCONNECTED, which is rethrown so the caller unwinds to its
	 * reconnect path.
	 */
	void obtain_mining(kj::WaitScope &ws, Init::Client &init)
	{
		try {
			auto req = init.makeMiningRequest();
			fill_context(req.initContext());
			mining_ = req.send().wait(ws).getResult();

			auto probe = mining_.isInitialBlockDownloadRequest();
			fill_context(probe.initContext());
			probe.send().wait(ws);

			have_mining_.store(true);
		} catch (const kj::Exception &e) {
			/* Never leave a half obtained cap behind: the keepalive loop
			 * decides what to call by have_mining_, and a non-null mining_
			 * with have_mining_ false would be a cap we never validated. */
			mining_ = Mining::Client(nullptr);
			have_mining_.store(false);
			if (e.getType() == kj::Exception::Type::DISCONNECTED)
				throw;
		}
	}

	/* Drop every capnp cap derived from the current client. Must run on the
	 * service thread while that client's RpcSystem is still alive, otherwise
	 * the later destruction of a cap dereferences a freed RpcSystem. */
	void reset_caps()
	{
		/*
		 * Clear the flag before the cap it advertises, and never leave the
		 * two disagreeing: have_mining_ is what every caller — including
		 * this thread's own keepalive probe — tests before calling through
		 * mining_, so a true flag over a null cap is a null dereference.
		 * The destructor runs this via executeSync on the service thread,
		 * which can only be inside the event loop, i.e. parked in the
		 * keepalive wait that is about to probe.
		 */
		have_mining_.store(false);
		connected_.store(false);
		mining_ = Mining::Client(nullptr);
		exec_thread_ = mp::Thread::Client(nullptr);
		callback_thread_ = mp::Thread::Client(nullptr);
		server_threadmap_ = mp::ThreadMap::Client(nullptr);
		our_threadmap_ = mp::ThreadMap::Client(nullptr);
	}

	void signal_started(bool &signalled)
	{
		if (signalled)
			return;
		signalled = true;
		{
			std::lock_guard<std::mutex> lock(mutex_);
			started_ = true;
		}
		cv_.notify_all();
	}

	std::string socket_path_;
	std::thread thread_;
	std::atomic<bool> stop_{false};
	std::atomic<bool> connected_{false};
	std::atomic<bool> have_mining_{false};

	bool started_ = false;
	std::mutex mutex_;
	std::condition_variable cv_;

	const kj::Executor *executor_ = nullptr;

	Mining::Client mining_{nullptr};
	mp::ThreadMap::Client our_threadmap_{nullptr};
	mp::ThreadMap::Client server_threadmap_{nullptr};
	mp::Thread::Client callback_thread_{nullptr};
	mp::Thread::Client exec_thread_{nullptr};
};

/* A template capability, owned by (and only touched on) the service thread. */
struct BlockTemplateHolder {
	MiningService *svc;
	BlockTemplate::Client client;
};

int MiningService::create_new_block(BlockTemplateHolder **out)
{
	auto *holder = run([this]() -> kj::Promise<BlockTemplateHolder *> {
		auto req = mining_.createNewBlockRequest();
		fill_context(req.initContext());
		return req.send().then([this](auto resp) {
			return new BlockTemplateHolder{ this, resp.getResult() };
		});
	});
	*out = holder;
	return holder ? 0 : -1;
}

} // namespace

/* --------------------------------------------------------------------------
 * C API. No exception may cross this boundary into ckpool's C code.
 * -------------------------------------------------------------------------- */

extern "C" {

mining_ipc_ctx *mining_ipc_connect(const char *socket_path)
{
	try {
		auto *impl = new MiningIpc(socket_path ? socket_path : "");
		if (!impl->connect()) {
			delete impl;
			return nullptr;
		}
		return reinterpret_cast<mining_ipc_ctx *>(impl);
	} catch (...) {
		return nullptr;
	}
}

void mining_ipc_disconnect(mining_ipc_ctx *ctx)
{
	delete reinterpret_cast<MiningIpc *>(ctx);
}

int mining_ipc_try_obtain_mining(mining_ipc_ctx *ctx)
{
	if (!ctx)
		return -1;
	try {
		return reinterpret_cast<MiningIpc *>(ctx)->try_obtain_mining() ? 0 : -1;
	} catch (...) {
		return -1;
	}
}

int mining_ipc_get_tip(mining_ipc_ctx *ctx, mining_tip *out_tip)
{
	if (!ctx || !out_tip)
		return -1;
	try {
		return reinterpret_cast<MiningIpc *>(ctx)->get_tip(out_tip);
	} catch (...) {
		return -1;
	}
}

int mining_ipc_wait_tip_changed(mining_ipc_ctx *ctx, const char *current_tip_hex,
				double timeout_seconds, mining_tip *out_new_tip)
{
	if (!ctx || !current_tip_hex || !out_new_tip)
		return -1;
	try {
		return reinterpret_cast<MiningIpc *>(ctx)->wait_tip_changed(current_tip_hex,
									    timeout_seconds, out_new_tip);
	} catch (...) {
		return -1;
	}
}

/* --- Phase 2: block template service --- */

mining_ipc_service *mining_ipc_service_connect(const char *socket_path)
{
	try {
		auto *svc = new MiningService(socket_path ? socket_path : "");
		svc->start();
		return reinterpret_cast<mining_ipc_service *>(svc);
	} catch (...) {
		return nullptr;
	}
}

void mining_ipc_service_disconnect(mining_ipc_service *svc)
{
	delete reinterpret_cast<MiningService *>(svc);
}

int mining_ipc_service_ready(mining_ipc_service *svc)
{
	if (!svc)
		return 0;
	return reinterpret_cast<MiningService *>(svc)->ready() ? 1 : 0;
}

int mining_ipc_create_new_block(mining_ipc_service *svc, mining_block_template **out)
{
	if (!svc || !out)
		return -1;
	auto *service = reinterpret_cast<MiningService *>(svc);
	if (!service->ready())
		return -1;
	try {
		BlockTemplateHolder *holder = nullptr;
		if (service->create_new_block(&holder) != 0 || !holder)
			return -1;
		*out = reinterpret_cast<mining_block_template *>(holder);
		return 0;
	} catch (...) {
		return -1;
	}
}

int mining_ipc_template_header(mining_block_template *t, unsigned char header[80])
{
	if (!t || !header)
		return -1;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		h->svc->run([h, header]() -> kj::Promise<void> {
			auto req = h->client.getBlockHeaderRequest();
			h->svc->fill_context(req.initContext());
			return req.send().then([header](auto resp) {
				auto data = resp.getResult();
				KJ_REQUIRE(data.size() == 80, "block header not 80 bytes");
				memcpy(header, data.begin(), 80);
			});
		});
		return 0;
	} catch (...) {
		return -1;
	}
}

int mining_ipc_template_coinbase(mining_block_template *t, mining_coinbase *out)
{
	if (!t || !out)
		return -1;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		memset(out, 0, sizeof(*out));
		h->svc->run([h, out]() -> kj::Promise<void> {
			auto req = h->client.getCoinbaseTxRequest();
			h->svc->fill_context(req.initContext());
			return req.send().then([out](auto resp) {
				auto cb = resp.getResult();
				out->version = cb.getVersion();
				out->sequence = cb.getSequence();
				out->block_reward_remaining = cb.getBlockRewardRemaining();
				out->lock_time = cb.getLockTime();

				auto ssp = cb.getScriptSigPrefix();
				KJ_REQUIRE(ssp.size() <= MINING_MAX_SCRIPTSIG, "script_sig_prefix too large");
				memcpy(out->script_sig_prefix, ssp.begin(), ssp.size());
				out->script_sig_prefix_len = ssp.size();

				auto wit = cb.getWitness();
				KJ_REQUIRE(wit.size() <= MINING_MAX_WITNESS, "witness too large");
				memcpy(out->witness, wit.begin(), wit.size());
				out->witness_len = wit.size();

				auto outs = cb.getRequiredOutputs();
				KJ_REQUIRE(outs.size() <= MINING_MAX_REQUIRED_OUTPUTS, "too many required outputs");
				out->required_outputs_count = outs.size();
				for (uint i = 0; i < outs.size(); i++) {
					auto o = outs[i];
					KJ_REQUIRE(o.size() <= MINING_MAX_OUTPUT, "required output too large");
					memcpy(out->required_output[i], o.begin(), o.size());
					out->required_output_len[i] = o.size();
				}
			});
		});
		return 0;
	} catch (...) {
		return -1;
	}
}

int mining_ipc_template_merkle_path(mining_block_template *t,
				    unsigned char branch[MINING_MAX_MERKLES][32], int *count)
{
	if (!t || !branch || !count)
		return -1;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		int n = h->svc->run([h, branch]() -> kj::Promise<int> {
			auto req = h->client.getCoinbaseMerklePathRequest();
			h->svc->fill_context(req.initContext());
			return req.send().then([branch](auto resp) -> int {
				auto path = resp.getResult();
				KJ_REQUIRE(path.size() <= MINING_MAX_MERKLES, "too many merkle branches");
				for (uint i = 0; i < path.size(); i++) {
					KJ_REQUIRE(path[i].size() == 32, "merkle branch not 32 bytes");
					memcpy(branch[i], path[i].begin(), 32);
				}
				return (int)path.size();
			});
		});
		*count = n;
		return 0;
	} catch (...) {
		return -1;
	}
}

int mining_ipc_template_block(mining_block_template *t, unsigned char **out,
			      size_t *out_len)
{
	if (!t || !out || !out_len)
		return -1;
	*out = nullptr;
	*out_len = 0;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		/* The response data is only valid inside the continuation, so copy
		 * it there — into malloc'd memory, which is not thread affine and
		 * so may be handed straight back to the (non-kj) caller. */
		struct BlockBuf {
			unsigned char *data;
			size_t len;
		};

		BlockBuf b = h->svc->run([h]() -> kj::Promise<BlockBuf> {
			auto req = h->client.getBlockRequest();
			h->svc->fill_context(req.initContext());
			return req.send().then([](auto resp) -> BlockBuf {
				auto data = resp.getResult();

				KJ_REQUIRE(data.size() > 80, "block shorter than a header");
				KJ_REQUIRE(data.size() <= MINING_MAX_BLOCK_BYTES,
					   "block exceeds MINING_MAX_BLOCK_BYTES");
				auto *buf = static_cast<unsigned char *>(malloc(data.size()));
				KJ_REQUIRE(buf != nullptr, "block allocation failed");
				memcpy(buf, data.begin(), data.size());
				return BlockBuf{ buf, data.size() };
			});
		});
		if (!b.data)
			return -1;
		*out = b.data;
		*out_len = b.len;
		return 0;
	} catch (...) {
		return -1;
	}
}

void mining_ipc_block_free(unsigned char *block)
{
	free(block);
}

int mining_ipc_submit_solution(mining_block_template *t, uint32_t version,
			       uint32_t timestamp, uint32_t nonce,
			       const unsigned char *coinbase, size_t coinbase_len, int *accepted)
{
	if (!t || !coinbase || !accepted)
		return -1;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		bool ok = h->svc->run([=]() -> kj::Promise<bool> {
			auto req = h->client.submitSolutionRequest();
			h->svc->fill_context(req.initContext());
			req.setVersion(version);
			req.setTimestamp(timestamp);
			req.setNonce(nonce);
			req.setCoinbase(kj::arrayPtr(reinterpret_cast<const capnp::byte *>(coinbase),
						     coinbase_len));
			return req.send().then([](auto resp) -> bool {
				return resp.getResult();
			});
		});
		*accepted = ok ? 1 : 0;
		return 0;
	} catch (...) {
		return -1;
	}
}

void mining_block_template_destroy(mining_block_template *t)
{
	if (!t)
		return;
	auto *h = reinterpret_cast<BlockTemplateHolder *>(t);
	try {
		/* The capability must be dropped on the service thread; dropping it
		 * releases the template server-side via capnp reference counting. */
		h->svc->run([h]() -> kj::Promise<void> {
			delete h;
			return kj::READY_NOW;
		});
	} catch (...) {
		/* If the service thread is gone we cannot free it cleanly; leak
		 * rather than risk touching capnp state off-thread. */
	}
}

/* --- Phase 2: Mining.checkBlock (JD candidate-block validation) --- */

int mining_ipc_check_block(mining_ipc_service *svc,
			   const unsigned char *block, size_t block_len,
			   const mining_block_check_options *options,
			   int *valid,
			   char *reason, size_t reason_sz,
			   char *debug, size_t debug_sz)
{
	if (!svc || !block || !block_len || !options || !valid)
		return -1;

	auto *service = reinterpret_cast<MiningService *>(svc);
	if (!service->ready())
		return -1;

	/* Snapshot options for the marshalled lambda (caller may free after return). */
	const bool check_merkle = options->check_merkle_root != 0;
	const bool check_pow = options->check_pow != 0;

	try {
		struct CheckResult {
			bool ok;
			std::string reason;
			std::string debug;
		};

		/* Copy block bytes before executeSync so the caller's buffer may
		 * be stack-local and reused; the promise captures the copy. */
		std::string block_copy(reinterpret_cast<const char *>(block), block_len);

		CheckResult r = service->run([service, block_copy, check_merkle, check_pow]()
					     -> kj::Promise<CheckResult> {
			auto req = service->mining().checkBlockRequest();
			service->fill_context(req.initContext());
			req.setBlock(kj::arrayPtr(
				reinterpret_cast<const capnp::byte *>(block_copy.data()),
				block_copy.size()));
			auto opts = req.initOptions();
			opts.setCheckMerkleRoot(check_merkle);
			opts.setCheckPow(check_pow);
			return req.send().then([](auto resp) -> CheckResult {
				CheckResult out;
				out.ok = resp.getResult();
				auto reason_t = resp.getReason();
				auto debug_t = resp.getDebug();
				out.reason = std::string(reason_t.cStr(), reason_t.size());
				out.debug = std::string(debug_t.cStr(), debug_t.size());
				return out;
			});
		});

		*valid = r.ok ? 1 : 0;
		if (reason && reason_sz) {
			if (r.reason.empty())
				reason[0] = '\0';
			else {
				std::snprintf(reason, reason_sz, "%s", r.reason.c_str());
				reason[reason_sz - 1] = '\0';
			}
		}
		if (debug && debug_sz) {
			if (r.debug.empty())
				debug[0] = '\0';
			else {
				std::snprintf(debug, debug_sz, "%s", r.debug.c_str());
				debug[debug_sz - 1] = '\0';
			}
		}
		return 0;
	} catch (...) {
		return -1;
	}
}

} // extern "C"
