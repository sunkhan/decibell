#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif

#include <iostream>
#include <iomanip>
#include <string>
#include <vector>
#include <thread>
#include <atomic>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <ctime>
#include "messages.pb.h"
#include "../common/net_utils.hpp"

using boost::asio::ip::tcp;

namespace ssl = boost::asio::ssl;

void send_packet(ssl::stream<tcp::socket>& socket, const chatproj::Packet& packet) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);
    boost::asio::write(socket, boost::asio::buffer(framed));
}

chatproj::Packet read_packet(ssl::stream<tcp::socket>& socket) {
    char header[4];
    boost::asio::read(socket, boost::asio::buffer(header, 4));
    uint32_t body_len = ntohl(*reinterpret_cast<uint32_t*>(header));

    std::vector<char> body(body_len);
    boost::asio::read(socket, boost::asio::buffer(body));

    chatproj::Packet packet;
    packet.ParseFromArray(body.data(), body_len);
    return packet;
}

int main() {
    try {
        boost::asio::io_context io_context;
        tcp::resolver resolver(io_context);
        auto endpoints = resolver.resolve("127.0.0.1", "8080");

        // Initialize SSL context for client
        ssl::context ctx(ssl::context::tlsv12_client);
        
        // Skip CA verification for the self-signed localhost certificate
        ctx.set_verify_mode(ssl::verify_none); 

        // Wrap the TCP socket in an SSL stream
        ssl::stream<tcp::socket> socket(io_context, ctx);

        // Connect the underlying TCP socket
        boost::asio::connect(socket.lowest_layer(), endpoints);
        
        // Perform the TLS handshake
        socket.handshake(ssl::stream_base::client);
        
        std::cout << "Connected securely to server.\n";

        std::cout << "1. Register\n2. Login\nSelect option: ";
        int option;
        std::cin >> option;

        std::string username, password;
        std::cout << "Username: "; std::cin >> username;
        std::cout << "Password: "; std::cin >> password;

        chatproj::Packet packet;
        if (option == 1) {
            packet.set_type(chatproj::Packet::REGISTER_REQ);
            auto* req = packet.mutable_register_req();
            req->set_username(username);
            req->set_password(password);
        } else {
            packet.set_type(chatproj::Packet::LOGIN_REQ);
            auto* req = packet.mutable_login_req();
            req->set_username(username);
            req->set_password(password);
        }

        send_packet(socket, packet);
        auto resp = read_packet(socket);

        if (resp.type() == chatproj::Packet::REGISTER_RES) {
            std::cout << "Result: " << resp.register_res().message() << "\n";
            return 0; // Exit after registration
        } 
        else if (resp.type() == chatproj::Packet::LOGIN_RES) {
            if (!resp.login_res().success()) {
                std::cout << "Login failed: " << resp.login_res().message() << "\n";
                return 0;
            }
            std::cout << "Login successful. Entering chat...\n";
        }

        std::cin.ignore(); // Clear the newline character left in the buffer by std::cin

        std::atomic<bool> running{true};

        // Thread 1: Listen for incoming messages
        std::thread receiver([&socket, &running]() {
            try {
                while (running) {
                    auto msg_packet = read_packet(socket);
                    if (msg_packet.type() == chatproj::Packet::CHAT_MSG) {
                        const auto& chat = msg_packet.chat_msg();
                        
                        std::time_t t = chat.timestamp();
                        std::tm tm_buf;
                        localtime_s(&tm_buf, &t);

                        std::string prefix = "\r[" + std::string(chat.sender()) + "]";
                        if (!chat.recipient().empty()) {
                            prefix = "\r[Private: " + std::string(chat.sender()) + " -> " + std::string(chat.recipient()) + "]";
                        } else if (chat.sender() == "SYSTEM") {
                            prefix = "\r[SYSTEM]";
                        } else {
                            // Show the channel name
                            prefix = "\r[#" + std::string(chat.channel()) + "] [" + std::string(chat.sender()) + "]";
                        }

                        std::cout << prefix << " [" << std::put_time(&tm_buf, "%H:%M:%S") << "]: " 
                                  << chat.content() << "\n> ";
                    }
                }
            } catch (std::exception&) {
                if (running) std::cout << "\nConnection closed by server.\n";
            }
        });

        std::string current_channel = "global";

        // Thread 2 (Main): Read user input and send
        std::string input;
        while (running) {
            std::cout << "> ";
            std::getline(std::cin, input);
            
            if (input == "/quit") {
                running = false;
                socket.lowest_layer().close();
                break;
            }

            if (!input.empty()) {
                chatproj::Packet chat_packet;
                
                // 1. Parse /join command
                if (input.rfind("/join ", 0) == 0) {
                    std::string target_channel = input.substr(6);
                    if (!target_channel.empty()) {
                        chat_packet.set_type(chatproj::Packet::JOIN_CHANNEL);
                        chat_packet.mutable_join_channel()->set_channel_name(target_channel);
                        send_packet(socket, chat_packet);
                        current_channel = target_channel;
                    }
                    continue; // Skip the rest of the loop
                }
                
                // 2. Prepare base CHAT_MSG for anything else
                chat_packet.set_type(chatproj::Packet::CHAT_MSG);
                auto* chat_req = chat_packet.mutable_chat_msg();
                chat_req->set_channel(current_channel);
                
                // 3. Parse /msg command
                if (input.rfind("/msg ", 0) == 0) {
                    size_t first_space = input.find(' ', 5);
                    if (first_space != std::string::npos) {
                        std::string target_user = input.substr(5, first_space - 5);
                        std::string actual_msg = input.substr(first_space + 1);
                        
                        chat_req->set_recipient(target_user);
                        chat_req->set_content(actual_msg);
                    } else {
                        std::cout << "Usage: /msg <username> <message>\n> ";
                        continue;
                    }
                } else {
                    // 4. Standard channel broadcast
                    chat_req->set_content(input);
                }
                
                send_packet(socket, chat_packet);
            }
        }

        if (receiver.joinable()) {
            receiver.join();
        }

    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}