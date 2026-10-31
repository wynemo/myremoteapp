// Disposable CI fixture only. This runs before password entry and never reads
// password values. Ordinary hosting does not use UI Automation.
#include <windows.h>
#include <uiautomation.h>
#include <wrl/client.h>
#include <chrono>
#include <string>
#include <thread>
using Microsoft::WRL::ComPtr;

static std::string utf8(BSTR text) {
    if (!text) return "";
    int size = WideCharToMultiByte(CP_UTF8, 0, text, SysStringLen(text), nullptr, 0, nullptr, nullptr);
    std::string result(size, '\0');
    WideCharToMultiByte(CP_UTF8, 0, text, SysStringLen(text), result.data(), size, nullptr, nullptr);
    return result;
}
static void write(HANDLE file, const std::string& line) {
    DWORD written = 0;
    WriteFile(file, line.data(), static_cast<DWORD>(line.size()), &written, nullptr);
}
static void dump(IUIAutomation* ui, IUIAutomationElement* root, HANDLE file) {
    ComPtr<IUIAutomationCondition> condition;
    ComPtr<IUIAutomationElementArray> elements;
    if (FAILED(ui->CreateTrueCondition(&condition)) ||
        FAILED(root->FindAll(TreeScope_Subtree, condition.Get(), &elements))) return;
    int length = 0;
    elements->get_Length(&length);
    for (int i = 0; i < length && i < 256; ++i) {
        ComPtr<IUIAutomationElement> element;
        if (FAILED(elements->GetElement(i, &element))) continue;
        BSTR name = nullptr, id = nullptr, cls = nullptr;
        BOOL password = FALSE, focus = FALSE;
        element->get_CurrentName(&name);
        element->get_CurrentAutomationId(&id);
        element->get_CurrentClassName(&cls);
        element->get_CurrentIsPassword(&password);
        element->get_CurrentHasKeyboardFocus(&focus);
        write(file, "name=" + utf8(name) + " id=" + utf8(id) + " class=" + utf8(cls) +
            " password=" + std::to_string(password) + " focus=" + std::to_string(focus) + "\n");
        SysFreeString(name); SysFreeString(id); SysFreeString(cls);
    }
}
static HRESULT find_password(IUIAutomation* ui, IUIAutomationElement* root, IUIAutomationElement** result) {
    VARIANT value; VariantInit(&value);
    value.vt = VT_BOOL; value.boolVal = VARIANT_TRUE;
    ComPtr<IUIAutomationCondition> condition;
    HRESULT hr = ui->CreatePropertyCondition(UIA_IsPasswordPropertyId, value, &condition);
    if (FAILED(hr)) return hr;
    return root->FindFirst(TreeScope_Descendants, condition.Get(), result);
}
extern "C" HRESULT remote_ci_prepare_password_field(const wchar_t* trace_path) {
    HRESULT init = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    if (FAILED(init)) return init;
    struct Cleanup { ~Cleanup() { CoUninitialize(); } } cleanup;
    HANDLE file = CreateFileW(trace_path, GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return HRESULT_FROM_WIN32(GetLastError());
    struct File { HANDLE h; ~File() { CloseHandle(h); } } trace{file};
    ComPtr<IUIAutomation> ui;
    HRESULT hr = CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&ui));
    if (FAILED(hr)) return hr;
    ComPtr<IUIAutomationElement> root, password;
    hr = ui->GetRootElement(&root);
    if (FAILED(hr)) return hr;
    write(file, "before preparing fixture focus (no credential entered)\n");
    dump(ui.Get(), root.Get(), file);
    for (int i = 0; i < 20; ++i) {
        hr = find_password(ui.Get(), root.Get(), &password);
        if (SUCCEEDED(hr) && password) break;
        std::this_thread::sleep_for(std::chrono::milliseconds(250));
    }
    if (!password) {
        // Some runner images initially expose only the Switch user button.
        VARIANT name; VariantInit(&name);
        name.vt = VT_BSTR; name.bstrVal = SysAllocString(L"Switch user");
        ComPtr<IUIAutomationCondition> condition;
        ComPtr<IUIAutomationElement> button;
        ui->CreatePropertyCondition(UIA_NamePropertyId, name, &condition);
        VariantClear(&name);
        if (condition && SUCCEEDED(root->FindFirst(TreeScope_Descendants, condition.Get(), &button)) && button) {
            ComPtr<IUIAutomationInvokePattern> invoke;
            if (SUCCEEDED(button->GetCurrentPatternAs(UIA_InvokePatternId, IID_PPV_ARGS(&invoke)))) invoke->Invoke();
        }
        for (int i = 0; i < 20; ++i) {
            hr = find_password(ui.Get(), root.Get(), &password);
            if (SUCCEEDED(hr) && password) break;
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
        }
    }
    dump(ui.Get(), root.Get(), file);
    if (!password) return HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    // Populate a possible username field, never the password field. The fixture
    // launcher preserves its runner environment when restarting as SYSTEM.
    wchar_t username[256] = {};
    GetEnvironmentVariableW(L"USERNAME", username, 256);
    VARIANT edit; VariantInit(&edit); edit.vt = VT_I4; edit.lVal = UIA_EditControlTypeId;
    ComPtr<IUIAutomationCondition> condition;
    ComPtr<IUIAutomationElementArray> edits;
    if (SUCCEEDED(ui->CreatePropertyCondition(UIA_ControlTypePropertyId, edit, &condition)) &&
        SUCCEEDED(root->FindAll(TreeScope_Descendants, condition.Get(), &edits))) {
        int count = 0; edits->get_Length(&count);
        for (int i = 0; i < count; ++i) {
            ComPtr<IUIAutomationElement> element; edits->GetElement(i, &element);
            BOOL secret = TRUE; element->get_CurrentIsPassword(&secret);
            if (secret) continue;
            ComPtr<IUIAutomationValuePattern> value;
            if (SUCCEEDED(element->GetCurrentPatternAs(UIA_ValuePatternId, IID_PPV_ARGS(&value)))) value->SetValue(username);
        }
    }
    return password->SetFocus();
}
